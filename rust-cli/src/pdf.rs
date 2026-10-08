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

/// Normalize mupdf-rs page text to PyMuPDF's `page.get_text()` shape.
///
/// `Page::text()` routes through `mupdf_print_stext_page_as_text`, which emits a
/// BLANK LINE after every content line. PyMuPDF's get_text() does not: it
/// returns one newline per line. Keeping those blanks changes every block's
/// `raw` text relative to the Python shell, shifting derived indices — and
/// `page_to_blocks` would treat each blank as a paragraph separator, producing
/// one block per line instead of one per paragraph.
fn normalize_pymupdf_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Default CPU tier: text per page. Returns [] on any failure — a corrupt or
/// unreadable PDF must degrade to "no pages", never abort the whole ingest.
///
/// When the CPU tier shows no usable text and a VLM hook is supplied, escalate
/// to the precision tier (§5.1 "two-tier"), mirroring
/// koiosbase/parsers/pdf.py parse_pdf(). Python reached for PyMuPDF to render
/// page PNGs; here we stay on the same MuPDF engine the CPU tier uses, so the
/// two tiers cannot disagree about page boundaries.
pub fn extract_pages_cpu(path: &Path) -> Vec<String> {
    extract_pages(path, None)
}

/// The two-tier extraction itself. `vlm` receives `(path, PNG bytes)` per page
/// and returns transcribed text, exactly the callable contract Python exposes
/// as `parse_pdf(..., vlm=...)` — README documents this as the way to handle
/// scanned PDFs, and Rust had no equivalent until now.
/// A vision tier: receives `(path, PNG bytes)` for a scanned page and returns
/// the transcribed text. Aliased because writing the full `dyn Fn` inline at
/// every call site trips clippy's type-complexity lint and buries the intent.
pub type VlmFn = dyn Fn(&str, &[u8]) -> Result<String, String>;

pub fn extract_pages(path: &Path, vlm: Option<&VlmFn>) -> Vec<String> {
    let cpu_pages = extract_pages_cpu_inner(path);
    let scanned = looks_scanned(&cpu_pages, 40);
    if let Some(f) = vlm.filter(|_| scanned || cpu_pages.is_empty()) {
        return render_pages_png(path)
            .into_iter()
            .enumerate()
            .map(|(i, png)| match f(&path.display().to_string(), &png) {
                Ok(t) => t,
                Err(e) => {
                    // Python lets the callable's exception abort the parse; here
                    // we degrade to no text for that page but keep going, since
                    // one bad page must not lose the rest of the document.
                    log_skip(&format!("vlm failed on page {}: {e}", i + 1));
                    String::new()
                }
            })
            .collect();
    }
    cpu_pages
}

/// Render each page to PNG bytes with MuPDF
fn render_pages_png(path: &Path) -> Vec<Vec<u8>> {
    let doc = match mupdf::Document::open(path) {
        Ok(d) => d,
        Err(e) => {
            log_skip(&e.to_string());
            return Vec::new();
        }
    };
    let n = doc.page_count().unwrap_or(0);
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        match doc
            .load_page(i)
            .and_then(|p| {
                p.to_pixmap(
                    &mupdf::Matrix::IDENTITY,
                    &mupdf::Colorspace::device_rgb(),
                    false,
                    true,
                )
            })
            .and_then(|pix| {
                let mut buf: Vec<u8> = Vec::new();
                pix.write_to(&mut buf, mupdf::ImageFormat::PNG).map(|_| buf)
            }) {
            Ok(bytes) => out.push(bytes),
            Err(e) => {
                log_skip(&e.to_string());
                out.push(Vec::new());
            }
        }
    }
    out
}

fn extract_pages_cpu_inner(path: &Path) -> Vec<String> {
    // MuPDF via mupdf-rs — the SAME engine PyMuPDF binds, rather than a second
    // independent parser. Page numbers are the provenance unit (§4.1), so this
    // walks pages individually exactly like PyMuPDF's
    // `for page in doc: page.get_text()`; a whole-document blob would stamp
    // every citation with the wrong page.
    let doc = match mupdf::Document::open(path) {
        Ok(d) => d,
        Err(e) => {
            log_skip(&e.to_string());
            return Vec::new();
        }
    };
    let n = doc.page_count().unwrap_or(0);
    let mut pages = Vec::with_capacity(n as usize);
    for i in 0..n {
        // Default options are EMPTY flags, which is what PyMuPDF's
        // `page.get_text()` uses — matching it keeps page text byte-identical
        // between the two shells instead of merely "similar".
        match doc
            .load_page(i)
            .and_then(|p| p.text(mupdf::TextExtractOptions::default()))
        {
            Ok(text) => pages.push(normalize_pymupdf_text(&text)),
            Err(e) => {
                // Degrade per page rather than aborting the document: one bad
                // page must not cost the vault the other 40.
                log_skip(&format!("page {}: {e}", i + 1));
                pages.push(String::new());
            }
        }
    }
    pages
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
/// True when `line` is the final non-empty line of `text` — i.e. nothing can
/// possibly follow it to serve as its body.
fn is_last_non_empty_line(text: &str, line: &str) -> bool {
    text.split('\n').map(str::trim).rfind(|l| !l.is_empty()) == Some(line)
}

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
pub fn page_to_blocks(doc_path: &str, page_no: usize, text: &str, start_ord: usize) -> Vec<Ev> {
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
    let classified: Vec<(bool, &str)> = lines.iter().map(|l| (is_heading(l), *l)).collect();
    for (is_h, line) in classified {
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
        // A heading is a heading only if it has body under it. Treating a lone
        // short line as one swallowed the entire page: the line went into
        // `heading`, `pending` stayed empty, and the final flush produced
        // nothing — so a whole page silently contributed zero blocks.
        //
        // That was latent while PDFs were text-only (a real page has several
        // lines), but the vision tier makes it load-bearing: a VLM commonly
        // transcribes a sparse page as exactly one short sentence, and then the
        // text we just paid a model to recover would be thrown away.
        if is_h && pending.is_empty() && !is_last_non_empty_line(text, s) {
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
/// Content hash of a file — mirrors koiosbase/parsers/pdf.py file_sha256().
///
/// Python hashes the WHOLE file with no cap (its `limit` argument defaults to
/// None after the 16MB-truncation bug was fixed), so this must too: a capped
/// hash would disagree with Python's `doc_hash` provenance value and the parse
/// cache key would silently collide across different files.
/// Parse-cache read — mirrors koiosbase/parsers/pdf.py load_cache().
///
/// Keyed by the FULL content hash, so a stale entry can never masquerade as the
/// current file. Corrupt JSON degrades to None rather than poisoning ingest.
pub fn load_cache(path: &Path, cache_dir: &Path) -> Option<serde_json::Value> {
    let f = cache_dir.join(format!("{}.json", file_sha256(path)));
    let text = std::fs::read_to_string(&f).ok()?;
    serde_json::from_str(&text).ok()
}

/// Parse-cache write — mirrors koiosbase/parsers/pdf.py save_cache().
pub fn save_cache(path: &Path, cache_dir: &Path, data: &serde_json::Value) -> std::io::Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let f = cache_dir.join(format!("{}.json", file_sha256(path)));
    std::fs::write(f, crate::json_python_dumps(data))
}

pub fn file_sha256(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    match std::fs::File::open(path) {
        Ok(mut f) => {
            let mut buf = [0u8; 1 << 20];
            loop {
                use std::io::Read;
                match f.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => h.update(&buf[..n]),
                    Err(_) => break,
                }
            }
        }
        Err(_) => return String::new(),
    }
    format!("{:x}", h.finalize())
}

/// Frontmatter for a parsed PDF, mirroring parsers/pdf.py's Document(...).
///
/// Two details that made the stored column differ from Python:
///   * `doc_hash` was missing entirely — it is the §4.1 provenance value and
///     the parse cache key, so it cannot be invented later;
///   * `ocr` was emitted as the string "false" where Python stores a real bool,
///     because `fm_to_json` stringifies every scalar. It is built as a JSON
///     object directly here so the types survive.
pub fn pdf_frontmatter(path: &Path, scanned: bool) -> String {
    let parser = if scanned { "pdf-vlm" } else { "pdf-cpu" };
    let mut map = serde_json::Map::new();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    map.insert("title".into(), serde_json::Value::String(stem));
    map.insert(
        "source".into(),
        serde_json::Value::String(path.display().to_string()),
    );
    map.insert(
        "parser".into(),
        serde_json::Value::String(parser.to_string()),
    );
    map.insert(
        "doc_hash".into(),
        serde_json::Value::String(format!("sha256:{}", file_sha256(path))),
    );
    map.insert("ocr".into(), serde_json::Value::Bool(scanned));
    crate::json_python_dumps(&serde_json::Value::Object(map))
}
