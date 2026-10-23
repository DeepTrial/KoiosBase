//! Per-vault caches for the long-lived MCP process.
//!
//! `koios mcp` serves many requests from one process, but every call rebuilt
//! the link graph by scanning the `links` table and re-deriving page
//! confidence. That is pure overhead when nothing changed between requests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use rusqlite::Connection;

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

/// Cheap fingerprint of the indexed corpus, used to invalidate caches.
///
/// Counting rows and summing hashes is far cheaper than rebuilding the graph,
/// and it changes whenever a document is added, removed, or edited.
pub fn corpus_stamp(conn: &Connection) -> String {
    let blocks: i64 = conn
        .query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0))
        .unwrap_or(0);
    let links: i64 = conn
        .query_row("SELECT COUNT(*) FROM links", [], |r| r.get(0))
        .unwrap_or(0);
    let hash_sum: String = conn
        .query_row(
            "SELECT COALESCE(SUM(LENGTH(COALESCE(hash,''))),0) FROM blocks",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0i64)
        .to_string();
    format!("{blocks}:{links}:{hash_sum}")
}

/// Load the weighted link graph, reusing a cached copy when the corpus has
/// not changed since it was built.
pub fn graph(vault: &std::path::Path, conn: &Connection) -> Result<Adjacency, String> {
    let stamp = corpus_stamp(conn);
    let key = vault.to_path_buf();
    {
        let m = GRAPHS.lock().map_err(|e| e.to_string())?;
        if let Some(c) = m.get(&key) {
            if c.stamp == stamp {
                return Ok(c.adj.clone());
            }
        }
    }
    let pc = crate::retrieval::build_page_confidence(conn);
    let adj = crate::retrieval::load_graph_weighted(conn, &pc).map_err(|e| e.to_string())?;
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
