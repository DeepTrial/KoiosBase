//! Compile layer: entities/ + sources/ pages (§5.3, §4.3) and the quality
//! gate (§5.4) — mirrors koiosbase/compile/{entities,sources,gate}.py.
//!
//! Two invariants carried over from the Python side:
//!   * compiled pages are Markdown truth in wiki/ and every fact must carry a
//!     wikilink back to raw (P6);
//!   * pages start at `confidence: draft` and are promoted only via
//!     cross-family verification or human confirmation — never citation
//!     counts (§5.4), which is what blocks the回流污染循环.

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;

use crate::pipeline::Ev;

static CAP_RUN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b([A-Z][A-Za-z0-9]*(?:\s+[A-Z][A-Za-z0-9]*)+)\b").unwrap());
static ALLCAPS: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]{2,}[A-Za-z0-9]*)\b").unwrap());
static ORG_SUFFIX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"([\x{4e00}-\x{9fff}]{2,8}(?:公司|集团|银行|研究院|大学|中心|部))").unwrap()
});
static CONFIDENCE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^confidence:\s*(\w+)").unwrap());
static TERMS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[A-Za-z0-9\x{4e00}-\x{9fff}]{3,}").unwrap());

const STOPWORDS: [&str; 17] = [
    "The",
    "This",
    "That",
    "However",
    "Therefore",
    "In",
    "On",
    "For",
    "Note",
    "Total",
    "Revenue",
    "Operating",
    "Cash",
    "Debt",
    "Net",
    "Gross",
    "Annual",
];

#[derive(Default)]
pub struct Entity {
    pub entity_id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub facts: Vec<(String, String)>, // (text, block_id)
}

pub fn slugify(name: &str) -> String {
    let mut s = String::new();
    for c in name.trim().chars() {
        if c.is_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(&c) {
            s.push(c);
        } else {
            s.push('-');
        }
    }
    let s = s.trim_matches('-').to_lowercase();
    if s.is_empty() {
        "entity".to_string()
    } else {
        s
    }
}

/// Cheap deterministic entity extraction (no LLM in the default path).
pub fn extract_entities(blocks: &[Ev]) -> HashMap<String, Entity> {
    let mut found: HashMap<String, Entity> = HashMap::new();
    for b in blocks {
        let mut names: Vec<String> = Vec::new();
        for m in CAP_RUN.find_iter(&b.raw) {
            names.push(m.as_str().to_string());
        }
        for m in ALLCAPS.find_iter(&b.raw) {
            names.push(m.as_str().to_string());
        }
        for m in ORG_SUFFIX.find_iter(&b.raw) {
            names.push(m.as_str().to_string());
        }
        for name in names {
            let name = name.trim().to_string();
            if name.is_empty() || name.chars().count() < 2 {
                continue;
            }
            if STOPWORDS.contains(&name.as_str()) {
                continue;
            }
            let eid = slugify(&name);
            let ent = found.entry(eid.clone()).or_insert(Entity {
                entity_id: eid,
                name: name.clone(),
                ..Default::default()
            });
            if name != ent.name && !ent.aliases.contains(&name) {
                ent.aliases.push(name);
            }
            let mut snippet = b.raw.trim().replace('\n', " ");
            if snippet.chars().count() > 160 {
                snippet = format!("{}…", snippet.chars().take(160).collect::<String>());
            }
            ent.facts.push((snippet, b.id.clone()));
        }
    }
    found
}

/// Which entity pages already exist for these entities (indexed lookup).
pub fn affected_entities(conn: &Connection, entities: &HashMap<String, Entity>) -> Vec<String> {
    let mut out = Vec::new();
    for eid in entities.keys() {
        let like = format!("%{eid}.md");
        let hit = conn
            .query_row(
                "SELECT 1 FROM blocks WHERE doc_path LIKE ? LIMIT 1",
                rusqlite::params![like],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if hit {
            out.push(eid.clone());
        }
    }
    out
}

/// Which concept/synthesis pages are near these blocks (summary lookup).
/// Deterministic stand-in for a vector search over page summaries.
pub fn affected_concepts(conn: &Connection, blocks: &[Ev], top_k: usize) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for b in blocks {
        for m in TERMS_RE.find_iter(&b.raw.to_lowercase()) {
            let t = m.as_str().to_string();
            if !terms.contains(&t) {
                terms.push(t);
            }
        }
    }
    if terms.is_empty() {
        return Vec::new();
    }
    let mut rows: Vec<(String, String)> = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT doc_path, summary FROM sections WHERE summary IS NOT NULL AND summary != ''",
    ) {
        if let Ok(m) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
            rows = m.filter_map(Result::ok).collect();
        }
    }
    let mut scored: Vec<(String, usize)> = rows
        .into_iter()
        .map(|(dp, sum)| {
            let hay = sum.to_lowercase();
            let hits = terms.iter().filter(|t| hay.contains(t.as_str())).count();
            (dp, hits)
        })
        .filter(|(_, h)| *h > 0)
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.into_iter().take(top_k).map(|(d, _)| d).collect()
}

const ENTITY_TEMPLATE: &str = r#"---
type: entity
entity_id: {eid}
aliases: [{aliases}]
confidence: draft
generated: true
last_compiled: {stamp}
sources: [{sources}]
---

# {name}

## 关键事实
{facts}

## 关系
{relations}

## 矛盾与未决
{conflicts}
"#;

pub fn render_entity_page(ent: &Entity, stamp: &str) -> String {
    let facts = if ent.facts.is_empty() {
        "- (暂无)".to_string()
    } else {
        ent.facts
            .iter()
            .take(8)
            .map(|(t, b)| format!("- {t} [[raw/{b}]]"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let relations = if ent.aliases.is_empty() {
        "- (暂无)".to_string()
    } else {
        ent.aliases
            .iter()
            .take(5)
            .map(|a| format!("- related → [[entities/{}]]", slugify(a)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let sources = ent
        .facts
        .iter()
        .take(3)
        .map(|(_, b)| format!("raw/{b}"))
        .collect::<Vec<_>>()
        .join(", ");
    ENTITY_TEMPLATE
        .replace("{eid}", &ent.entity_id)
        .replace(
            "{aliases}",
            &ent.aliases
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        )
        .replace("{stamp}", stamp)
        .replace("{sources}", &sources)
        .replace("{name}", &ent.name)
        .replace("{facts}", &facts)
        .replace("{relations}", &relations)
        .replace("{conflicts}", "- (无)")
}

pub fn write_entity_pages(
    vault: &Path,
    entities: &HashMap<String, Entity>,
    stamp: &str,
) -> std::io::Result<usize> {
    let out_dir = vault.join("wiki").join("entities");
    std::fs::create_dir_all(&out_dir)?;
    let mut n = 0;
    // sorted for deterministic output ordering across shells
    let mut keys: Vec<&String> = entities.keys().collect();
    keys.sort();
    for k in keys {
        let ent = &entities[k];
        std::fs::write(
            out_dir.join(format!("{}.md", ent.entity_id)),
            render_entity_page(ent, stamp),
        )?;
        n += 1;
    }
    Ok(n)
}

// ---------------------------------------------------------------- sources/ --

const SOURCE_TEMPLATE: &str = r#"---
type: source
title: {title}
source: raw/{rel}
confidence: draft
generated: true
last_compiled: {stamp}
---

# {title}

源文档：`raw/{rel}`

## 导读摘要
{summary}

## 本文档能回答的问题
{questions}

## 覆盖章节
{sections}
"#;

fn build_summary(blocks: &[Ev], max_chars: usize) -> String {
    let para: Vec<String> = blocks
        .iter()
        .map(|b| b.raw.replace('\n', " ").trim().to_string())
        .collect();
    let joined = para.join(" ");
    // Mirror Python re.split(r"(?<=[。！？；.!?;])\s*", joined): a break AFTER
    // every terminator (the ASCII '.' included, so "7.5" splits into "7." and
    // "5 亿元。"), then `out += s + " "` while len(out)+len(s) <= max_chars.
    // The trailing space is inside the budget check, which is why the Python
    // output can end with two spaces — reproduced verbatim here.
    let mut pieces: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in joined.chars() {
        cur.push(c);
        if "。！？；.!?;".contains(c) {
            pieces.push(cur.clone());
            cur.clear();
        }
    }
    if !cur.is_empty() {
        pieces.push(cur);
    }
    let mut out = String::new();
    for s in &pieces {
        // Python's `\s*` in the split pattern consumes the whitespace BETWEEN
        // pieces, so each piece arrives with no leading space. Without this
        // trim_start the Rust version emits a double space.
        let s = s.trim_start();
        if out.chars().count() + s.chars().count() > max_chars {
            break;
        }
        out.push_str(s);
        out.push(' ');
    }
    let out = out.trim().to_string();
    if out.is_empty() {
        joined.chars().take(max_chars).collect()
    } else {
        out
    }
}

pub fn write_source_pages(
    vault: &Path,
    docs: &[(String, String, Vec<Ev>)], // (rel_path, title, blocks)
    stamp: &str,
    sections_by_doc: &HashMap<String, Vec<(String, String)>>, // title, section_id
) -> std::io::Result<usize> {
    let out_dir = vault.join("wiki").join("sources");
    std::fs::create_dir_all(&out_dir)?;
    let mut n = 0;
    for (rel, title, blocks) in docs {
        if blocks.is_empty() {
            continue;
        }
        let name = Path::new(rel)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| rel.clone());
        let summary = build_summary(blocks, 280);
        let secs = sections_by_doc.get(rel).cloned().unwrap_or_default();
        let questions: Vec<String> = secs
            .iter()
            .filter(|(t, _)| !t.starts_with("__") && !t.starts_with("p."))
            .take(5)
            .map(|(t, sid)| format!("- 关于「{t}」，本文档有哪些说明？ ({sid})"))
            .collect();
        let questions = if questions.is_empty() {
            blocks
                .iter()
                .take(5)
                .map(|b| {
                    let last = b.breadcrumb.split(" > ").last().unwrap_or("").to_string();
                    // Python appends the block id — a question with no address
                    // cannot be traced back to the evidence it came from, which
                    // is the whole point of §4.3 navigation pages.
                    format!("- 关于「{last}」，有哪些内容？ ({})", b.id)
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            questions.join("\n")
        };
        let sections = if secs.is_empty() {
            "- (无显式标题)".to_string()
        } else {
            secs.iter()
                .filter(|(t, _)| !t.starts_with("__"))
                .map(|(t, sid)| format!("- {t} (`{sid}`)"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let page = SOURCE_TEMPLATE
            .replace("{title}", title)
            .replace("{rel}", rel)
            .replace("{stamp}", stamp)
            .replace("{summary}", &summary)
            .replace("{questions}", &questions)
            .replace("{sections}", &sections);
        std::fs::write(out_dir.join(format!("{name}.md")), page)?;
        n += 1;
    }
    Ok(n)
}

// ------------------------------------------------------------------- gate --

pub const VALID_CONF: [&str; 3] = ["draft", "medium", "high"];
/// §6.6: unverified pages confer NO authority — cuts the
/// "citations -> authority -> more citations" self-reinforcing loop.
pub fn edge_weight(confidence: &str) -> f64 {
    match confidence {
        "draft" => 0.0,
        "medium" => 0.5,
        "high" => 1.0,
        _ => 0.0,
    }
}

pub fn read_confidence(path: &Path) -> String {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return "draft".to_string(),
    };
    match CONFIDENCE_RE.captures(&text) {
        Some(m) if VALID_CONF.contains(&&m[1]) => m[1].to_string(),
        _ => "draft".to_string(),
    }
}

/// Promote only via cross-family verification or human confirmation (§5.4).
pub fn can_promote(confidence: &str, verified: bool, human_confirmed: bool) -> bool {
    if !VALID_CONF.contains(&confidence) || confidence == "high" {
        return false;
    }
    verified || human_confirmed
}

/// Raise one step: draft -> medium -> high. Returns the new confidence.
pub fn promote(path: &Path, verified: bool, human_confirmed: bool) -> std::io::Result<String> {
    let mut text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Ok("draft".to_string()),
    };
    let cur = read_confidence(path);
    if !can_promote(&cur, verified, human_confirmed) {
        return Ok(cur);
    }
    let nxt = if cur == "draft" { "medium" } else { "high" };
    if CONFIDENCE_RE.is_match(&text) {
        text = CONFIDENCE_RE
            .replace(&text, format!("confidence: {nxt}").as_str())
            .to_string();
    } else {
        text = text.replacen("---\n", &format!("---\nconfidence: {nxt}\n"), 1);
    }
    std::fs::write(path, text)?;
    Ok(nxt.to_string())
}

const ANSWER_TEMPLATE: &str = r#"---
type: answer
confidence: draft
generated: true
created: {stamp}
sources: [{sources}]
---

# {question}

{answer}

## 依据
{evidence}
"#;

/// Answer write-back (§6.6): the approved answer becomes a draft page.
pub fn write_answer_page(
    vault: &Path,
    question: &str,
    answer: &str,
    evidence: &[Ev],
    stamp: &str,
) -> std::io::Result<std::path::PathBuf> {
    let out_dir = vault.join("wiki").join("answers");
    std::fs::create_dir_all(&out_dir)?;
    let slug = slugify(&question.chars().take(60).collect::<String>());
    let slug = if slug.is_empty() {
        "answer".to_string()
    } else {
        slug
    };
    let target = out_dir.join(format!("{slug}.md"));
    let ev = if evidence.is_empty() {
        "- (无)".to_string()
    } else {
        evidence
            .iter()
            .take(8)
            .map(|b| format!("- [[raw/{}]]", b.id))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let sources = evidence
        .iter()
        .take(3)
        .map(|b| format!("raw/{}", b.id))
        .collect::<Vec<_>>()
        .join(", ");
    let page = ANSWER_TEMPLATE
        .replace("{stamp}", stamp)
        .replace("{question}", question)
        .replace("{answer}", answer)
        .replace("{sources}", &sources)
        .replace("{evidence}", &ev);
    std::fs::write(&target, page)?;
    Ok(target)
}
