//! ACL / multi-tenancy (design doc §9.2) — mirrors koiosbase/security/acl.py.
//!
//! The rule that matters: **filter at retrieval time, not after generation.**
//! Masking an answer that has already been composed leaks in paraphrase. So the
//! check must sit in the recall path, before a block can reach the context.
//!
//! ACLs are declared in frontmatter (`acl: [finance-team]`), so Markdown stays
//! the source of truth (P1) and permissions are versioned with the content.

use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;

/// An empty/absent acl means unrestricted.
pub fn grants_for(conn: &Connection, doc_path: &str) -> Option<Vec<String>> {
    let fm: String = conn
        .query_row(
            "SELECT frontmatter FROM documents WHERE path=?",
            rusqlite::params![doc_path],
            |r| r.get(0),
        )
        .ok()?;
    let v: Value = serde_json::from_str(&fm).ok()?;
    let acl = v.get("acl")?;
    let list = if let Some(s) = acl.as_str() {
        vec![s.to_string()]
    } else {
        acl.as_array()?
            .iter()
            .map(|x| x.to_string().trim_matches('"').to_string())
            .collect()
    };
    if list.is_empty() {
        None
    } else {
        Some(list)
    }
}

/// True if the principal may see this document.
pub fn allowed(acl: Option<&Vec<String>>, principal_groups: Option<&[String]>) -> bool {
    match acl {
        None => true, // public document
        Some(acl) => match principal_groups {
            None => false, // restricted doc, anonymous caller
            Some(g) => acl.iter().any(|a| g.contains(a)),
        },
    }
}

/// Does ANY document declare an acl? Cheap pre-check mirroring _any_restricted.
fn any_restricted(conn: &Connection) -> bool {
    let mut stmt = match conn.prepare("SELECT frontmatter FROM documents") {
        Ok(s) => s,
        Err(_) => return false,
    };
    let rows: Vec<String> = match stmt.query_map([], |r| r.get(0)) {
        Ok(m) => m.filter_map(Result::ok).collect(),
        Err(_) => return false,
    };
    rows.iter().any(|fm| {
        serde_json::from_str::<Value>(fm)
            .ok()
            .and_then(|v| v.get("acl").cloned())
            .map(|a| !a.is_null())
            .unwrap_or(false)
    })
}

/// Drop blocks the principal is not entitled to — before context assembly.
/// This must be called on EVERY retrieval path (see pipeline::retrieve_channel).
pub fn filter_blocks(
    conn: &Connection,
    blocks: Vec<crate::pipeline::Ev>,
    principal_groups: Option<&[String]>,
) -> Vec<crate::pipeline::Ev> {
    if principal_groups.is_none() && !any_restricted(conn) {
        return blocks;
    }
    let mut cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let mut out = Vec::new();
    for b in blocks {
        let grants = cache
            .entry(b.doc_path.clone())
            .or_insert_with(|| grants_for(conn, &b.doc_path))
            .clone();
        if allowed(grants.as_ref(), principal_groups) {
            out.push(b);
        }
    }
    out
}

/// Which documents this principal can see (used by Studio exports).
pub fn visible_paths(
    conn: &Connection,
    principal_groups: Option<&[String]>,
) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT path FROM documents")?;
    let paths: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .filter_map(Result::ok)
        .collect();
    Ok(paths
        .into_iter()
        .filter(|p| allowed(grants_for(conn, p).as_ref(), principal_groups))
        .collect())
}
