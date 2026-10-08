//! Knowledge state machine + cascading correction (§8.2, §8.3) —
//! mirrors koiosbase/state/model.py.
//!
//! P5 — knowledge has state, and a correction propagates. Marking a block
//! retracted must reverse-index every page citing it and mark those stale.

use rusqlite::{params, Connection};
use std::collections::HashMap;

pub const VALID_STATES: [&str; 5] = ["active", "superseded", "disputed", "retracted", "draft"];
/// States excluded from retrieval by default (§8.2 disposal column).
pub const FILTERED: [&str; 2] = ["superseded", "retracted"];

/// Days since the Unix epoch — used for both `now()` and TTL age arithmetic.
pub fn now_ts_days() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64
        / 86400
}

pub fn now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Python uses datetime.now(timezone.utc).isoformat(timespec="seconds");
    // reproduced here so timestamps in the db have the same shape.
    let days = secs / 86400;
    let rem = secs % 86400;
    let ymd = civil_from_days(days as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
        ymd.0,
        ymd.1,
        ymd.2,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Date-only stamp `YYYY-MM-DD` — matches compile_vault()/stamp_now() on the
/// Python side, which use `datetime.now(utc).date().isoformat()`.
pub fn now_date() -> String {
    let (y, m, d) = civil_from_days(now_ts_days());
    format!("{y:04}-{m:02}-{d:02}")
}

/// Inverse of civil_from_days — needed to turn `valid_from: YYYY-MM-DD` into a
/// day number for TTL comparison. Pure integer, portable to any target.
pub fn days_from_civil(y: i64, m: u64, d: u64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as i64;
    era * 146097 + doe - 719468
}

/// Howard Hinnant's civil_from_days — pure integer, no libc dependency, so this
/// stays portable to the Windows target used by the release workflow.
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u64;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u64;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn set_block_state(
    conn: &Connection,
    block_id: &str,
    st: &str,
    reason: &str,
) -> Result<(), String> {
    if !VALID_STATES.contains(&st) {
        return Err(format!("invalid state: {st}"));
    }
    conn.execute(
        "INSERT OR REPLACE INTO block_state(block_id,state,reason,updated) VALUES(?,?,?,?)",
        params![block_id, st, reason, now()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn get_block_state(conn: &Connection, block_id: &str) -> String {
    conn.query_row(
        "SELECT state FROM block_state WHERE block_id=?",
        params![block_id],
        |r| r.get::<_, String>(0),
    )
    .unwrap_or_else(|_| "active".to_string())
}

/// Reverse index: which pages cite this block (the blast radius, §8.3).
///
/// Reads the vault's wiki/ Markdown directly when a vault path is given —
/// compiled pages are DERIVED and deliberately kept out of the block index
/// (P1), so querying only indexed rows would find zero references and silently
/// swallow the cascade.
pub fn referencing_pages(
    conn: &Connection,
    block_id: &str,
    vault: Option<&std::path::Path>,
) -> Vec<String> {
    let marker = format!("[[raw/{block_id}]]");
    if let Some(v) = vault {
        let root = v.join("wiki");
        let mut pages = Vec::new();
        if root.exists() {
            let mut stack = vec![root.clone()];
            while let Some(dir) = stack.pop() {
                if let Ok(rd) = std::fs::read_dir(&dir) {
                    for e in rd.flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            stack.push(p);
                        } else if p.extension().and_then(|x| x.to_str()) == Some("md") {
                            if let Ok(text) = std::fs::read_to_string(&p) {
                                if text.contains(&marker) {
                                    let rel = p
                                        .strip_prefix(v)
                                        .unwrap_or(&p)
                                        .to_string_lossy()
                                        .to_string();
                                    pages.push(rel);
                                }
                            }
                        }
                    }
                }
            }
        }
        pages.sort();
        return pages;
    }
    let like = format!("%{marker}%");
    conn.prepare("SELECT DISTINCT doc_path FROM blocks WHERE layer='wiki' AND raw LIKE ?")
        .and_then(|mut s| {
            s.query_map(params![like], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default()
}

pub fn mark_stale(conn: &Connection, page_path: &str, reason: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO page_state(page_path,stale,state,reason,updated) \
         VALUES(?,1,'active',?,?)",
        params![page_path, reason, now()],
    )?;
    Ok(())
}

pub fn is_stale(conn: &Connection, page_path: &str) -> bool {
    conn.query_row(
        "SELECT stale FROM page_state WHERE page_path=?",
        params![page_path],
        |r| r.get::<_, i64>(0),
    )
    .map(|v| v != 0)
    .unwrap_or(false)
}

pub fn cascade_retraction(
    conn: &Connection,
    block_id: &str,
    reason: &str,
    vault: Option<&std::path::Path>,
) -> Result<Vec<String>, String> {
    set_block_state(conn, block_id, "retracted", reason)?;
    let pages = referencing_pages(conn, block_id, vault);
    for p in &pages {
        mark_stale(conn, p, &format!("cited retracted {block_id}: {reason}"))
            .map_err(|e| e.to_string())?;
    }
    Ok(pages)
}

/// Drop blocks whose source block is superseded/retracted (§8.2).
pub fn filter_visible(
    conn: &Connection,
    blocks: Vec<crate::pipeline::Ev>,
) -> Vec<crate::pipeline::Ev> {
    blocks
        .into_iter()
        .filter(|b| !FILTERED.contains(&get_block_state(conn, &b.id).as_str()))
        .collect()
}

/// The §8.3 table itself, shared by single and batch lookups.
fn disposition_of(stale: i64, state: &str, high_risk: bool) -> &'static str {
    if state == "retracted" {
        return "hard_filter";
    }
    if state == "superseded" {
        return "filtered_unless_explicit_history";
    }
    if stale != 0 {
        return if high_risk {
            "refuse_or_recompile"
        } else {
            "downrank_and_async_recompile"
        };
    }
    "use"
}

/// Query-time disposition for a page (§8.3 table, lazy synthesis update).
pub fn disposition_for(conn: &Connection, page_path: &str, high_risk: bool) -> &'static str {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT stale,state FROM page_state WHERE page_path=?",
            params![page_path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    match row {
        Some((stale, state)) => disposition_of(stale, &state, high_risk),
        None => "use",
    }
}

/// Batch the §8.3 lookup — `disposition_for` is one row, queries see many.
/// Mirrors koiosbase/state/model.py page_dispositions().
pub fn page_dispositions(
    conn: &Connection,
    page_paths: &[String],
) -> HashMap<String, &'static str> {
    let mut out: HashMap<String, &'static str> = HashMap::new();
    for p in page_paths {
        out.insert(p.clone(), disposition_for(conn, p, false));
    }
    out
}

/// §8.3 on the read path: stale pages sink, retracted pages vanish.
///
/// `downrank_and_async_recompile` means literally that — evidence from a page
/// whose sources moved on is pushed behind fresh evidence rather than being
/// presented as equally current.
///
/// This was MISSING from every Rust read path (search / ask / MCP / eval): the
/// Python `retrieve()` applies it after `filter_visible`, so `koios retract`
/// had an observable effect on one shell and not the other.
pub fn apply_disposition(
    conn: &Connection,
    blocks: Vec<crate::pipeline::Ev>,
    high_risk: bool,
) -> Vec<crate::pipeline::Ev> {
    if blocks.is_empty() {
        return blocks;
    }
    let mut paths: Vec<String> = Vec::new();
    for b in &blocks {
        if !paths.contains(&b.doc_path) {
            paths.push(b.doc_path.clone());
        }
    }
    let disp = page_dispositions(conn, &paths);
    let mut keep: Vec<crate::pipeline::Ev> = Vec::new();
    let mut stale: Vec<crate::pipeline::Ev> = Vec::new();
    for b in blocks {
        match disp.get(&b.doc_path).copied().unwrap_or("use") {
            "hard_filter" => continue,
            "downrank_and_async_recompile" | "refuse_or_recompile" => stale.push(b),
            _ => keep.push(b),
        }
    }
    keep.extend(stale);
    let _ = high_risk;
    keep
}
