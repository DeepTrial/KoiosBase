//! PDF adapter — parse(path) -> Document (design doc §5.1).
//!
//! Mirrors koiosbase/parsers/pdf.py. The CPU tier is implemented here; the VLM
//! precision tier stays a *hook* (as it is on the Python side), so v0.2 ships
//! with no model dependency while keeping the escalation path real.
//!
//! Provenance: every Block records `page_range` so citations can point at an
//! exact page (§4.1, §6.5).

use std::path::Path;

use crate::pipeline::Ev;

pub const HEADING_MAX_LEN: usize = 80;

/// Default CPU tier: text per page. Returns [] on any failure — a corrupt or
/// unreadable PDF must degrade to "no pages", never abort the whole ingest.
pub fn extract_pages_cpu(path: &Path) -> Vec<String> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            log_skip(&e.to_string());
            return Vec::new();
        }
    };
    match pdf_extract::extract_text_from_mem_by_pages(&bytes) {
        // Per-page, exactly like PyMuPDF's `for page in doc: page.get_text()`.
        // Page numbers ARE the provenance unit (§4.1), so a whole-document blob
        // would put wrong page numbers in every citation.
        Ok(pages) => pages,
        Err(e) => {
            log_skip(&e.to_string());
            Vec::new()
        }
    }
}

/// Record why a tier was skipped — silent degradation is undebuggable.
fn log_skip(msg: &str) {
    eprintln!("[pdf] tier skipped: {msg}");
}

/// Heuristic for "this PDF is probably a scan": almost no extractable text.
/// This is what decides whether to escalate to the VLM tier (§5.1).
pub fn looks_scanned(page_texts: &[String], min_chars: usize) -> bool {
    if page_texts.is_empty() {
        return true;
    }
    let usable = page_texts
        .iter()
        .filter(|t| t.trim().chars().count() >= min_chars)
        .count();
    usable < std::cmp::max(1, page_texts.len() / 2)
}

/// Distinguish a heading from body text in flat PDF extracts.
///
/// A page extract has no Markdown structure, so surface cues are all there is:
/// a short line with no sentence-ending punctuation. Deliberately conservative
/// — calling body text a heading DROPS the block entirely (this exact bug made
/// PDF pages index as empty on the Python side).
fn is_heading(line: &str) -> bool {
    let s = line.trim();
    if s.is_empty() || s.chars().count() > HEADING_MAX_LEN {
        return false;
    }
    if s.ends_with(['.', '。', ';', '；', ':', '：']) {
        return false;
    }
    s.split_whitespace().count() <= 8
}

/// Turn one page's text into Blocks tagged with its page_range (§4.1).
pub fn page_to_blocks(
    doc_path: &str,
    page_no: usize,
    text: &str,
    start_ord: usize,
) -> Vec<Ev> {
    let mut blocks: Vec<Ev> = Vec::new();
    let mut ordinal = start_ord;
    let base_title = match doc_path.rsplit_once('.') {
        Some((stem, _)) => stem.to_string(),
        None => doc_path.to_string(),
    };
    let mut heading: Option<String> = None;
    let mut pending: Vec<&str> = Vec::new();

    let mut flush = |pending: &mut Vec<&str>, heading: &Option<String>| -> Option<Ev> {
        let body: String = pending
            .iter()
            .map(|x| x.trim())
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        pending.clear();
        if body.is_empty() {
            return None;
        }
        ordinal += 1;
        let crumb = match heading {
            Some(h) => format!("{base_title} > {h}"),
            None => format!("{base_title} > p.{page_no}"),
        };
        Some(Ev {
            id: format!("{doc_path}#p{page_no}/{ordinal}"),
            raw: body.clone(),
            breadcrumb: crumb,
            doc_path: doc_path.to_string(),
        })
    };

    let mut lines: Vec<&str> = text.split('\n').collect();
    // borrow-checker-friendly two-pass: collect (is_heading, line) first
    let classified: Vec<(bool, &str)> = lines
        .iter()
        .map(|l| (is_heading(l), *l))
        .collect();
    for (is_h, line) in classified {
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
        if is_h && pending.is_empty() {
            heading = Some(s.to_string());
            continue;
        }
        pending.push(s);
    }
    if let Some(b) = flush(&mut pending, &heading) {
        blocks.push(b);
    }
    let _ = &mut lines;
    blocks
}

/// Frontmatter the Python adapter writes for a PDF doc (§5.1 provenance).
pub fn pdf_frontmatter(path: &Path, scanned: bool) -> String {
    let parser = if scanned { "pdf-vlm" } else { "pdf-cpu" };
    format!(
        "title: {}\nsource: {}\nparser: {}\nocr: {}",
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        path.display(),
        parser,
        scanned
    )
}
