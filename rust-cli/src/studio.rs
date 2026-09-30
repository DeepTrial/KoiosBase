//! Studio-style outputs (§13 v1.0): brief / mindmap — mirrors studio/exports.py.
//!
//! These are *projections* of knowledge that already exists: they compile
//! nothing new and add no assertions of their own. Every generated statement
//! carries the citation it came from (P6 applies to exports too), and an export
//! with no evidence says so instead of inventing filler.

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::Connection;
use std::path::Path;

use crate::pipeline::Ev;

static BRACKET_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[()\[\]]").unwrap());

const BRIEF_TEMPLATE: &str = r#"---
type: brief
generated: true
created: {stamp}
sources: [{sources}]
---

# {title}

> 由 KoiosBase 生成 · 共 {n} 条证据 · 生成时间 {stamp}

## 要点
{bullets}

## 证据清单
{evidence}
"#;

const MINDMAP_TEMPLATE: &str = r#"---
type: mindmap
generated: true
created: {stamp}
---

# {title}

```mermaid
mindmap
  root(({root}))
{branches}
```
"#;

/// Split on sentence terminators, keeping the punctuation (Python's
/// `(?<=[。！？；.!?;])\s*`), then take the first `limit` non-empty pieces.
fn sentences(text: &str, limit: usize) -> Vec<String> {
    let flat = text.replace('\n', " ");
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in flat.chars() {
        cur.push(c);
        if "。！？；.!?;".contains(c) {
            let s = cur.trim().to_string();
            if !s.is_empty() {
                out.push(s);
            }
            cur.clear();
            if out.len() >= limit && limit > 0 {
                // keep scanning? Python slices after the split, so stop is fine
                break;
            }
        }
    }
    let tail = cur.trim().to_string();
    if !tail.is_empty() && out.len() < limit {
        out.push(tail);
    }
    out.truncate(limit);
    out
}

pub fn render_brief(title: &str, blocks: &[Ev], stamp: &str) -> String {
    let mut bullets: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for b in blocks {
        for s in sentences(&b.raw, 2) {
            let key: String = s.chars().take(40).collect();
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            bullets.push(format!("- {s} [[raw/{}]]", b.id));
        }
    }
    let evidence = blocks
        .iter()
        .take(20)
        .map(|b| format!("- `{}` — {}", b.id, b.breadcrumb))
        .collect::<Vec<_>>()
        .join("\n");
    let sources = blocks
        .iter()
        .take(5)
        .map(|b| format!("raw/{}", b.id))
        .collect::<Vec<_>>()
        .join(", ");
    BRIEF_TEMPLATE
        .replace("{stamp}", stamp)
        .replace("{title}", title)
        .replace("{n}", &blocks.len().to_string())
        .replace(
            "{bullets}",
            if bullets.is_empty() {
                "- (无证据)".to_string()
            } else {
                bullets
                    .iter()
                    .take(12)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            .as_str(),
        )
        .replace(
            "{evidence}",
            if evidence.is_empty() {
                "- (无)"
            } else {
                &evidence
            },
        )
        .replace("{sources}", &sources)
}

pub fn render_mindmap(
    title: &str,
    root: &str,
    branches: &[(String, Vec<String>)],
    stamp: &str,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (head, leaves) in branches.iter().take(12) {
        let safe = BRACKET_RE.replace_all(head, " ").trim().to_string();
        let safe = if safe.is_empty() {
            "topic".to_string()
        } else {
            safe
        };
        lines.push(format!("  {safe}"));
        for leaf in leaves.iter().take(6) {
            let lsafe = BRACKET_RE.replace_all(leaf, " ").trim().to_string();
            if !lsafe.is_empty() {
                lines.push(format!("    {lsafe}"));
            }
        }
    }
    let root_clean = BRACKET_RE.replace_all(root, " ").trim().to_string();
    let root_out = if root_clean.is_empty() {
        root.to_string()
    } else {
        root_clean
    };
    MINDMAP_TEMPLATE
        .replace("{stamp}", stamp)
        .replace("{title}", title)
        .replace("{root}", &root_out)
        .replace(
            "{branches}",
            if lines.is_empty() {
                "  (空)".to_string()
            } else {
                lines.join("\n")
            }
            .as_str(),
        )
}

/// Build a brief from hybrid recall on `topic`.
pub fn studio_brief(
    conn: &Connection,
    vault: &Path,
    topic: &str,
    top: usize,
    groups: Option<&[String]>,
) -> std::io::Result<std::path::PathBuf> {
    // ACL FIRST. An export is written to disk, so a leak here outlives the
    // request and cannot be recalled. Anonymous callers previously received a
    // restricted HR figure verbatim because this path did no filtering at all.
    let blocks = crate::pipeline::retrieve_channel(conn, topic, top, groups);
    let blocks = crate::acl::filter_blocks(conn, blocks, groups);
    let out = vault.join("wiki").join("synthesis");
    std::fs::create_dir_all(&out)?;
    let stamp = crate::state::now_date();
    let slug = crate::compile::slugify(&topic.chars().take(40).collect::<String>());
    let slug = if slug.is_empty() {
        "brief".to_string()
    } else {
        slug
    };
    let target = out.join(format!("brief-{slug}.md"));
    std::fs::write(&target, render_brief(topic, &blocks, &stamp))?;
    Ok(target)
}

/// Build a mindmap from the section tree — structure, not new claims.
pub fn studio_mindmap(
    conn: &Connection,
    vault: &Path,
    root: &str,
    groups: Option<&[String]>,
) -> std::io::Result<std::path::PathBuf> {
    let mut rows: Vec<(String, String, Option<String>)> = Vec::new();
    // A section TITLE is content: 「薪酬」 alone discloses that pay data exists.
    // Same rationale as Python's visible_doc_paths — filter the tree, not just
    // the prose.
    let visible = crate::acl::visible_paths(conn, groups).unwrap_or_default();
    let visible_ref: Vec<String> = visible;
    if let Ok(mut stmt) =
        conn.prepare("SELECT doc_path,title,parent_id FROM sections ORDER BY doc_path, ordinal")
    {
        if let Ok(m) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        }) {
            rows = m
                .filter_map(Result::ok)
                .filter(|r| visible_ref.contains(&r.0))
                .collect();
        }
    }
    // insertion-ordered grouping (Python dict preserves first-seen order)
    let mut keys: Vec<String> = Vec::new();
    let mut by_parent: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (dp, title, parent) in rows {
        let key = parent.unwrap_or(dp);
        if !by_parent.contains_key(&key) {
            keys.push(key.clone());
        }
        by_parent.entry(key).or_default().push(title);
    }
    let branches: Vec<(String, Vec<String>)> = keys
        .into_iter()
        .take(12)
        .map(|k| {
            let v = by_parent.get(&k).cloned().unwrap_or_default();
            (k, v)
        })
        .collect();
    let out = vault.join("wiki").join("synthesis");
    std::fs::create_dir_all(&out)?;
    let stamp = crate::state::now_date();
    let slug = crate::compile::slugify(&root.chars().take(40).collect::<String>());
    let slug = if slug.is_empty() {
        "map".to_string()
    } else {
        slug
    };
    let target = out.join(format!("mindmap-{slug}.md"));
    std::fs::write(&target, render_mindmap(root, root, &branches, &stamp))?;
    Ok(target)
}
