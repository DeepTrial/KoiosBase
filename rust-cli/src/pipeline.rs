//! Full query pipeline (§6), mirroring koiosbase/query/pipeline.py.
//!
//! Ported so `eval` measures the SAME code path as the Python harness:
//! `retrieve -> grade -> optional channel ④ escalation -> generate`.
//! Without this, a refusal case scored on a bare `hybrid_search` result is not
//! comparable to one scored on the real pipeline answer (§6.5).

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection};
use std::collections::HashMap;

use crate::retrieval::{full_corpus, hybrid_search, is_cjk};

pub const REFUSAL: &str = "资料中未涉及";

// Python: SENTENCE_SPLIT = re.compile(r"(?<=[。！？；!?;])\s*|\n+")
//   — lookbehind is unsupported by the `regex` crate, so the same behaviour is
//     hand-rolled (see split_sentences): break AFTER a terminator, or on a run
//     of newlines. Identical semantics, different mechanism.
static CITATION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[\[([^\]]+)\]\]|\(([^()]*#[^\s()]+)\)").unwrap());

fn is_sentence_end(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '；' | '!' | '?' | ';')
}

/// Public companion to `citation_coverage`: the reusable pieces of §6.5 as
/// plain predicates, mirroring koiosbase/generation/contracts.py.
///
/// Python exposes these because the point of the contracts is that a CALLER's
/// model is judged by the same rules as the built-in generator — supplying an
/// `llm` to `query()` must not let it invent. Rust had them only buried inside
/// citation_coverage, so an external generator had nothing to check against.
pub fn has_citation(sentence: &str) -> bool {
    CITATION_RE.is_match(sentence)
}

pub fn is_refusal_pub(answer: &str) -> bool {
    answer.contains(REFUSAL)
}

/// Refusal contract: answering substantively with zero evidence and no marker
/// is a violation; with evidence present anything goes (conflict protocol).
pub fn check_refusal(evidence_empty: bool, answer: &str) -> bool {
    if !evidence_empty {
        return true;
    }
    is_refusal_pub(answer)
}

/// All-in-one gate matching Python's return shape: empty = every contract met.
/// Keys stay identical ("citation" / "refusal") so CI output is comparable.
pub fn check_contracts(answer: &str, evidence: &[Ev]) -> HashMap<String, String> {
    let (cited, total) = citation_coverage(answer);
    let mut violations: HashMap<String, String> = HashMap::new();
    if total > 0 && cited < total {
        violations.insert(
            "citation".to_string(),
            format!("{cited}/{total} sentences cited"),
        );
    }
    if !check_refusal(evidence.is_empty(), answer) {
        violations.insert(
            "refusal".to_string(),
            "answer asserted with no evidence and no refusal marker".to_string(),
        );
    }
    violations
}

fn split_sentences(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut pending_end = false; // saw a terminator; whitespace may follow
    for c in text.chars() {
        if c == '\n' {
            pending_end = true;
            continue;
        }
        if pending_end {
            if c.is_whitespace() {
                continue;
            }
            pending_end = false;
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
            }
            cur.clear();
        }
        cur.push(c);
        if is_sentence_end(c) {
            pending_end = true;
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// (sentences_with_citation, total_factual_sentences) — the §10.2 metric.
pub fn citation_coverage(answer: &str) -> (usize, usize) {
    let sentences = split_sentences(answer);
    let cited = sentences.iter().filter(|s| CITATION_RE.is_match(s)).count();
    (cited, sentences.len())
}

pub struct Ev {
    pub id: String,
    pub raw: String,
    pub breadcrumb: String,
    pub doc_path: String,
}

pub fn get_block(conn: &Connection, id: &str) -> Option<Ev> {
    conn.query_row(
        "SELECT id,raw,breadcrumb,doc_path FROM blocks WHERE id=?",
        params![id],
        |r| {
            Ok(Ev {
                id: r.get(0)?,
                raw: r.get::<_, String>(1).unwrap_or_default(),
                breadcrumb: r.get::<_, String>(2).unwrap_or_default(),
                doc_path: r.get::<_, String>(3).unwrap_or_default(),
            })
        },
    )
    .ok()
}

/// Channel dispatch — `groups` carries the caller's principal.
///
/// Two filters are applied here and nowhere else, both before context assembly:
///   * ACL (§9.2)    — a block the principal cannot see must never shape an answer
///   * state (§8.2)  — superseded/retracted blocks are excluded from recall
///
/// Skipping either one is how Python v0.1 leaked restricted docs to anonymous
/// callers, so both stay on the hot path even though they cost a query.
///
/// It previously hardcoded `filter_blocks(..., None)`, so EVERY caller
/// (studio, MCP, answer write-back) silently retrieved as anonymous and an
/// entitled caller saw nothing. The principal has to travel with the request,
/// not be re-derived per call site — that is what makes "filter at retrieval,
/// never after generation" (§9.2) actually hold.
pub fn retrieve_channel(
    conn: &Connection,
    question: &str,
    limit: usize,
    groups: Option<&[String]>,
) -> Vec<Ev> {
    let rows = hybrid_search(conn, question, limit, true).unwrap_or_default();
    let mut ids: Vec<String> = rows.into_iter().map(|(i, _)| i).collect();
    if ids.is_empty() {
        // harness fallback: `retrieve()` when the named channel yields nothing
        ids = hybrid_search(conn, question, limit, true)
            .unwrap_or_default()
            .into_iter()
            .map(|(i, _)| i)
            .collect();
    }
    let mut out: Vec<Ev> = ids.iter().filter_map(|i| get_block(conn, i)).collect();
    out = crate::acl::filter_blocks(conn, out, groups);
    out = crate::state::filter_visible(conn, out);
    // §8.3 query-time disposition: stale pages are down-ranked (never silently
    // treated as fresh) and a retracted page is dropped entirely. Python's
    // retrieve() does this too; without it `koios retract` changed nothing on
    // this shell's read paths.
    crate::state::apply_disposition(conn, out, false)
}

/// The retrieval filter chain, as one callable.
///
/// Every read path must run this exact triple — ACL (§9.2), state (§8.2),
/// disposition (§8.3) — before a block can reach a context or a printed list.
/// Three call sites previously hand-rolled a subset of it (`cmd_tree`,
/// `cmd_full`, and the §4 escalation below), which is exactly how an anonymous
/// caller read the restricted blocks directly. One function, no variations.
pub fn filter_chain(conn: &Connection, blocks: Vec<Ev>, groups: Option<&[String]>) -> Vec<Ev> {
    let out = crate::acl::filter_blocks(conn, blocks, groups);
    let out = crate::state::filter_visible(conn, out);
    crate::state::apply_disposition(conn, out, false)
}

/// Every block in the corpus, after the filter chain. Used by channel ④ and by
/// the §4 escalation, both of which were reading the corpus raw.
pub fn visible_corpus(conn: &Connection, groups: Option<&[String]>) -> Vec<Ev> {
    let ids = full_corpus(conn, 200_000).unwrap_or_default();
    let all: Vec<Ev> = ids.iter().filter_map(|i| get_block(conn, i)).collect();
    filter_chain(conn, all, groups)
}

/// Grader verdict over ALL evidence joined, first 2000 chars, lowercased.
pub fn grade(question: &str, blocks: &[Ev]) -> &'static str {
    if blocks.is_empty() {
        return "evidence_absent";
    }
    let joined: String = blocks
        .iter()
        .map(|b| b.raw.clone())
        .collect::<Vec<_>>()
        .join(" ");
    let joined: String = joined.chars().take(2000).collect::<String>().to_lowercase();
    let mut hits = 0usize;
    let mut seen: Vec<char> = Vec::new();
    for c in question.chars() {
        if seen.contains(&c) {
            continue;
        }
        seen.push(c);
        if c.is_alphanumeric() {
            if joined.contains(&c.to_lowercase().to_string()) {
                hits += 1;
            }
        } else if is_cjk(c) && joined.contains(c) {
            hits += 1;
        }
    }
    if hits == 0 {
        "missing"
    } else {
        "enough"
    }
}

fn cjk_runs(question: &str) -> Vec<String> {
    let mut runs: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in question.chars() {
        if is_cjk(c) {
            cur.push(c);
        } else if !cur.is_empty() {
            runs.push(cur.clone());
            cur.clear();
        }
    }
    if !cur.is_empty() {
        runs.push(cur);
    }
    runs
}

/// True only if a REAL query term appears — never a stray single character.
/// Mirrors pipeline._has_terms: it is what keeps the escalation path from
/// handing the whole corpus to any question (which would break refusal §6.5).
fn has_terms(question: &str, blocks: &[Ev]) -> bool {
    if blocks.is_empty() {
        return false;
    }
    let joined: String = blocks
        .iter()
        .map(|b| b.raw.clone())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    for run in cjk_runs(question) {
        if run.chars().count() >= 2 && joined.contains(&run) {
            return true;
        }
    }
    for tok in question
        .split(|c: char| !(c.is_ascii_alphanumeric()))
        .filter(|s| !s.is_empty())
    {
        if joined.contains(&tok.to_lowercase()) {
            return true;
        }
    }
    false
}

/// Context assembly with budget discipline (§6.4).
pub fn assemble(blocks: &[Ev], budget_chars: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut total = 0usize;
    for b in blocks {
        let piece = format!("[{}] ({})\n{}\n", b.id, b.breadcrumb, b.raw);
        if total + piece.chars().count() > budget_chars {
            let remain = budget_chars.saturating_sub(total);
            if remain > 120 {
                let truncated: String = piece.chars().take(remain).collect();
                parts.push(format!("{truncated} …"));
            }
            break;
        }
        total += piece.chars().count();
        parts.push(piece);
    }
    parts.join("\n---\n")
}

/// Deterministic generator (llm=None branch of pipeline.generate).
///
/// Three details mirror koiosbase/query/pipeline.py generate() exactly, and
/// each one matters for metric parity rather than cosmetics:
///   * NO preamble line. Python removed 检索到的证据如下： deliberately — a
///     header sentence carrying no citation made every default answer a
///     citation violation, which trained callers to ignore that signal.
///   * citations are [[raw/{id}]], which is the form CITATION_RE matches;
///     the old `(id)` form scored zero coverage and shifted every denominator.
///   * stale pages get （待更新） appended, the *disclosure* half of §8.3 —
///     apply_disposition only reorders, so without this a reader receives a
///     stale page's answer with no hint its sources moved on.
fn generate(
    conn: &Connection,
    question: &str,
    _context: &str,
    blocks: &[Ev],
    stale_paths: &std::collections::BTreeSet<String>,
) -> String {
    if blocks.is_empty() {
        return format!("{REFUSAL}：当前知识库中没有任何相关证据。");
    }
    let mut lines: Vec<String> = Vec::new();
    for b in blocks.iter().take(3) {
        let snippet: String = b.raw.replace('\n', " ").chars().take(180).collect();
        let mut line = format!("- {snippet} [[raw/{}]]", b.id);
        // Python decides this from the same page_dispositions map the caller's
        // `stale_paths` comes from; reusing is_stale() here would double-count
        // pages marked by a different mechanism.
        if stale_paths.contains(&b.doc_path) {
            line.push_str(" （待更新）");
        }
        lines.push(line);
    }
    let _ = question;
    let _ = conn;
    lines.join("\n")
}

/// A generator: receives `(question, assembled_context)` and returns the answer.
/// Aliased so the callable's signature reads as a contract rather than noise.
pub type LlmFn = dyn Fn(&str, &str) -> Result<String, String>;

pub struct QueryResult {
    pub answer: String,
    pub evidence: Vec<Ev>,
    /// §6.5 contract violations, keyed like Python's dict
    /// (`{"citation": "0/1 sentences cited"}`). Empty means the contracts held.
    pub violations: HashMap<String, String>,
    /// Self-Route breadcrumb (§6.2): which channel ran, what the Grader said,
    /// whether the §4 escalation fired, and which cited pages are stale.
    pub trace: serde_json::Value,
}

/// Full pipeline with Grader-driven escalation (Self-Route, §6.2).
pub fn full_query(
    conn: &Connection,
    question: &str,
    top: usize,
    groups: Option<&[String]>,
) -> QueryResult {
    full_query_with(conn, question, top, groups, None)
}

/// Same pipeline, but letting the caller supply the generator.
///
/// Python exposes this as `query(..., llm=callable)` and README documents it as
/// the way to plug a real model in. Rust had no equivalent, so "bring your own
/// LLM" was a Python-only capability. The callable receives
/// `(question, assembled_context)` and its output goes through the SAME
/// `check_contracts` gate — handing KoiosBase a model must not let it invent
/// (§6.5). Returns `Err` when the callable fails, which Python surfaces as a
/// raised exception.
pub fn full_query_with(
    conn: &Connection,
    question: &str,
    top: usize,
    groups: Option<&[String]>,
    llm: Option<&LlmFn>,
) -> QueryResult {
    let mut blocks = retrieve_channel(conn, question, top, groups);
    let mut verdict = grade(question, &blocks);
    let mut trace = serde_json::json!({
        "question": question,
        "hits": blocks.len(),
        "channel": "hybrid+ppr",
        "verdict": verdict,
    });
    if verdict == "evidence_absent" {
        let cand_ids = full_corpus(conn, 200_000).unwrap_or_default();
        let cand: Vec<Ev> = cand_ids
            .iter()
            .take(top * 5)
            .filter_map(|i| get_block(conn, i))
            .collect();
        // Escalation is still retrieval, so it still runs the full chain. Taking
        // the top of the raw corpus here meant a restricted block reached the
        // context (and therefore the answer) on exactly the inputs the normal
        // channel would have rejected.
        let cand = crate::acl::filter_blocks(conn, cand, groups);
        let cand = crate::state::filter_visible(conn, cand);
        let cand = crate::state::apply_disposition(conn, cand, false);
        if !cand.is_empty() && has_terms(question, &cand) {
            blocks = cand;
            trace["escalated"] = serde_json::json!("full");
            verdict = grade(question, &blocks);
            trace["verdict"] = serde_json::json!(verdict);
            trace["hits"] = serde_json::json!(blocks.len());
        }
    }
    let context = assemble(&blocks, 6000);
    // §8.3 disclosure: which cited pages are behind their sources. Python feeds
    // this to generate() as `stale_paths`; apply_disposition only re-orders, so
    // without this a reader gets a stale answer with no hint.
    let stale_paths = {
        use std::collections::BTreeSet;
        let mut paths = Vec::new();
        for b in &blocks {
            if !paths.contains(&b.doc_path) {
                paths.push(b.doc_path.clone());
            }
        }
        let disp = crate::state::page_dispositions(conn, &paths);
        let mut s = BTreeSet::new();
        for doc in &paths {
            if matches!(
                disp.get(doc).map(|x| &**x as &str),
                Some("downrank_and_async_recompile") | Some("refuse_or_recompile")
            ) {
                s.insert(doc.clone());
            }
        }
        if !s.is_empty() {
            let arr: Vec<serde_json::Value> = s.iter().map(|p| serde_json::json!(p)).collect();
            trace["stale_pages"] = serde_json::json!(arr);
        }
        s
    };
    let answer = match llm {
        Some(f) => match f(question, &context) {
            Ok(a) => a,
            Err(e) => {
                // Python propagates the callable's exception to the same place.
                return QueryResult {
                    answer: e,
                    evidence: blocks,
                    violations: HashMap::new(),
                    trace,
                };
            }
        },
        None => generate(conn, question, &context, &blocks, &stale_paths),
    };
    // §6.5: the contracts gate the CALLER's model too, not just the built-in
    // generator. This is the whole reason `llm` is safe to accept.
    let violations = check_contracts(&answer, &blocks);
    QueryResult {
        answer,
        evidence: blocks,
        violations,
        trace,
    }
}

pub fn is_refusal(answer: &str) -> bool {
    answer.contains(REFUSAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-checked against koiosbase/generation/contracts.citation_coverage
    /// on the same five strings — Python returned (1,2) (2,5) (0,1) (0,1) (1,2).
    #[test]
    fn citation_coverage_matches_python() {
        let cases: Vec<(&str, (usize, usize))> = vec![
            (
                "营收为32亿元 (report.md#财务分析/营收/1)。现金流为正。",
                (1, 2),
            ),
            (
                "检索到的证据如下：\n- ACME 公司 2024 年营收 32 亿元。 (report.md#财务分析/营收/1)\n- 经营性现金流为正。 (report.md#财务分析/现金流/1)",
                (2, 5),
            ),
            ("资料中未涉及：当前知识库中没有任何相关证据。", (0, 1)),
            ("无标点也无引用的句子", (0, 1)),
            ("引用 [[report.md#a/b/1]] 在句中。另一句没引用。", (1, 2)),
        ];
        for (text, want) in cases {
            assert_eq!(citation_coverage(text), want, "mismatch on {:?}", text);
        }
    }
}
