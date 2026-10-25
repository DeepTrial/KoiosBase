//! Per-vault caches for the long-lived MCP process.
//!
//! `koios mcp` serves many requests from one process, but every call rebuilt
//! the link graph by scanning the `links` table and re-deriving page
//! confidence. That is pure overhead when nothing changed between requests.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

/// Cached adjacency, tagged with the schema state it was derived from so an
/// index rebuild invalidates it.
pub struct CachedGraph {
    /// Fingerprint of the corpus at the time this was built.
    pub stamp: String,
    pub adj: HashMap<String, Vec<(String, f64)>>,
}

pub type Adjacency = HashMap<String, Vec<(String, f64)>>;

static GRAPHS: Lazy<Mutex<HashMap<PathBuf, CachedGraph>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn hash_text(hasher: &mut Sha256, value: &str) {
    // Length-prefix every string: concatenating variable-length fields would
    // make different row layouts produce the same digest input.
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

/// Cryptographic fingerprint of every input to graph shape and edge weight.
///
/// Ordered rows and length-prefixed fields make the digest deterministic across
/// processes. Full raw content is included rather than trusting blocks.hash,
/// which is intentionally truncated; links and document frontmatter cover
/// target edits and confidence promotions even when all row counts stay fixed.
pub fn corpus_stamp(conn: &Connection) -> rusqlite::Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"koios-weighted-graph-v1\0");

    hasher.update(b"blocks\0");
    let mut stmt = conn.prepare(
        "SELECT id,COALESCE(doc_path,''),COALESCE(layer,'raw'),\
         COALESCE(raw,''),COALESCE(hash,'') FROM blocks ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (id, doc_path, layer, raw, hash) = row?;
        for value in [&id, &doc_path, &layer, &raw, &hash] {
            hash_text(&mut hasher, value);
        }
    }
    drop(stmt);

    hasher.update(b"links\0");
    let mut stmt = conn.prepare("SELECT src,dst,kind,weight FROM links ORDER BY src,dst,kind,weight")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, f64>(3)?,
        ))
    })?;
    for row in rows {
        let (src, dst, kind, weight) = row?;
        hash_text(&mut hasher, &src);
        hash_text(&mut hasher, &dst);
        hash_text(&mut hasher, &kind);
        hasher.update(weight.to_bits().to_be_bytes());
    }
    drop(stmt);

    hasher.update(b"documents\0");
    let mut stmt = conn.prepare(
        "SELECT path,COALESCE(frontmatter,'') FROM documents ORDER BY path",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (path, frontmatter) = row?;
        hash_text(&mut hasher, &path);
        hash_text(&mut hasher, &frontmatter);
    }

    let mut stamp = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(&mut stamp, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(stamp)
}

/// Load the weighted link graph, reusing a cached copy when the corpus has
/// not changed since it was built.
pub fn graph(vault: &std::path::Path, conn: &Connection) -> Result<Adjacency, String> {
    // Fingerprinting, confidence derivation, and link loading must see one SQLite
    // snapshot. Otherwise a concurrent index can pair an old stamp with a new
    // graph (or vice versa), making the wrong generation look fresh afterwards.
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let stamp = corpus_stamp(&tx).map_err(|e| e.to_string())?;
    let key = vault.to_path_buf();
    let cached = {
        let m = GRAPHS.lock().map_err(|e| e.to_string())?;
        m.get(&key)
            .filter(|cached| cached.stamp == stamp)
            .map(|cached| cached.adj.clone())
    };
    if let Some(adj) = cached {
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(adj);
    }

    let pc = crate::retrieval::build_page_confidence(&tx);
    let adj = crate::retrieval::load_graph_weighted(&tx, &pc).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    let mut m = GRAPHS.lock().map_err(|e| e.to_string())?;
    m.insert(
        key,
        CachedGraph {
            stamp,
            adj: adj.clone(),
        },
    );
    Ok(adj)
}

/// Forget everything cached for a vault — call after `koios index`.
pub fn invalidate(vault: &std::path::Path) {
    if let Ok(mut m) = GRAPHS.lock() {
        m.remove(&vault.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_corpus_uses_the_cached_adjacency() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE blocks(id TEXT,doc_path TEXT,layer TEXT,raw TEXT,hash TEXT);\
             CREATE TABLE links(src TEXT,dst TEXT,kind TEXT,weight REAL);\
             CREATE TABLE documents(path TEXT,frontmatter TEXT);",
        )
        .unwrap();
        let key = std::env::temp_dir().join(format!("koios_cache_reuse_{}", std::process::id()));
        invalidate(&key);
        assert!(graph(&key, &conn).unwrap().is_empty());

        // A sentinel that is not derivable from the empty tables proves the
        // second call returned the cache instead of rebuilding the graph.
        GRAPHS
            .lock()
            .unwrap()
            .get_mut(&key)
            .unwrap()
            .adj
            .insert("cache-hit".to_string(), Vec::new());
        assert!(graph(&key, &conn).unwrap().contains_key("cache-hit"));
        invalidate(&key);
    }
}
