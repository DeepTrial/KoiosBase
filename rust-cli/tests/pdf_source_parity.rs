//! Index parity when raw/ holds a NON-Markdown source.
//!
//! `collect_raw_docs` used to skip every extension except `md`, so a PDF in
//! raw/ contributed blocks but never got its sources/ navigation page (§4.3)
//! while Python built one from every raw doc via `parse_any`. Every downstream
//! count disagreed: 1 block against 5.
//!
//! The helper itself had unit tests; they stayed green because they only ever
//! fed it Markdown. The fixture bytes come from tests/pdf_sources.rs's minimal
//! PDF builder, since mupdf's authoring API is not reachable here.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use koios::connect;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-pdf-src-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    for d in [
        "raw",
        "wiki/sources",
        "wiki/entities",
        "wiki/concepts",
        "wiki/synthesis",
        "wiki/answers",
        ".index",
    ] {
        fs::create_dir_all(p.join(d)).unwrap();
    }
    p
}

/// A one-page PDF whose content stream draws a short text line. Copied from
/// tests/pdf_sources.rs so the two tests exercise the same byte stream.
fn minimal_pdf(text: &str) -> Vec<u8> {
    let esc = text
        .replace('\\', r"\\")
        .replace('(', r"\(")
        .replace(')', r"\)");
    let content = format!("BT /F1 12 Tf 72 720 Td ({esc}) Tj ET\n");
    let objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{}endstream",
            content.len(),
            content
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];

    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{}\nendobj\n", i + 1, body));
    }
    let xref = out.len();
    out.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objs.len() + 1
    ));
    for off in &offsets {
        out.push_str(&format!("{:010} 00000 n \n", off));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
        objs.len() + 1,
        xref
    ));
    out.into_bytes()
}

/// Regression guard: a non-Markdown raw doc must still get a sources/ page.
///
/// The failure mode was silent — nothing errored, the derived navigation entry
/// for the PDF simply never existed, so any count of "pages we can navigate"
/// differed between shells while both reported success.
#[test]
fn pdf_in_raw_gets_a_derived_source_page() {
    let p = vault("pdf");
    let pdf_path = p.join("raw").join("fin.pdf");
    fs::write(&pdf_path, minimal_pdf("ACME revenue was 3.2 billion yuan.")).unwrap();

    let conn = connect(&p).unwrap();
    let n = koios::index_pdf(&conn, "fin.pdf", &pdf_path).unwrap();
    assert!(n > 0, "the PDF produced no blocks");

    let (docs, sections) = koios::collect_raw_docs(&conn, &p).unwrap();
    let rels: Vec<&str> = docs.iter().map(|(r, _, _)| r.as_str()).collect();
    assert!(
        rels.contains(&"fin.pdf"),
        "collect_raw_docs skipped the PDF, so no sources/ page can be generated: {rels:?}"
    );

    let written = koios::compile::write_source_pages(&p, &docs, "2026-01-01", &sections).unwrap();
    assert_eq!(written, 1, "expected exactly one sources/ page for the PDF");
    let page = fs::read_to_string(p.join("wiki/sources/fin.md")).unwrap();
    assert!(page.contains("raw/fin.pdf"), "no provenance line:\n{page}");
    // Python appends the block address to each fallback question; a question
    // with no address cannot be traced back to its evidence.
    assert!(page.contains("(fin.pdf#p1/1)"), "question lost its address:\n{page}");

    let _ = fs::remove_dir_all(&p);
}

/// Title resolution for non-Markdown docs comes from the adapter, not from
/// reading the file as UTF-8 text — a PDF read as text is garbage.
#[test]
fn non_markdown_docs_resolve_their_title_from_the_adapter_row() {
    let p = vault("title");
    let pdf_path = p.join("raw").join("fin.pdf");
    fs::write(&pdf_path, minimal_pdf("ACME revenue was 3.2 billion yuan.")).unwrap();

    let conn = connect(&p).unwrap();
    koios::index_pdf(&conn, "fin.pdf", &pdf_path).unwrap();

    let (docs, _) = koios::collect_raw_docs(&conn, &p).unwrap();
    let title = docs
        .iter()
        .find(|(r, _, _)| r == "fin.pdf")
        .map(|(_, t, _)| t.clone())
        .expect("fin.pdf missing");
    assert_eq!(title, "fin", "title must come from the file stem: {title:?}");

    let _ = fs::remove_dir_all(&p);
}

/// `documents.frontmatter` must hold a JSON object with typed values, not a
/// bag of strings: Python stores `ocr` as a real bool and `doc_hash` as the
/// §4.1 provenance hash.
#[test]
fn pdf_frontmatter_is_typed_json_with_provenance_hash() {
    let p = vault("fm");
    let pdf_path = p.join("raw").join("fin.pdf");
    fs::write(&pdf_path, minimal_pdf("ACME revenue was 3.2 billion yuan.")).unwrap();

    let conn = connect(&p).unwrap();
    koios::index_pdf(&conn, "fin.pdf", &pdf_path).unwrap();
    let fm: String = conn
        .query_row(
            "SELECT frontmatter FROM documents WHERE path='fin.pdf'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    let v: serde_json::Value = serde_json::from_str(&fm).expect("frontmatter is not JSON");
    // Whether the heuristic calls this tiny fixture scanned is not the point —
    // the point is that `ocr` is a REAL bool and not the string "true"/"false"
    // that stringifying every scalar used to produce.
    assert!(
        matches!(v["ocr"], serde_json::Value::Bool(_)),
        "ocr must be a JSON bool, not a string: {fm}"
    );
    assert_eq!(
        v["parser"].as_str().unwrap_or("").starts_with("pdf-"),
        true,
        "{fm}"
    );
    assert!(
        v["doc_hash"].as_str().unwrap_or("").starts_with("sha256:"),
        "missing provenance hash: {fm}"
    );
    // Python's json.dumps default separators (", " / ": "), matched verbatim.
    assert!(fm.contains("\", \""), "wrong JSON separators: {fm}");

    let _ = fs::remove_dir_all(&p);
}

/// Unused import guard — keeps this file lint-clean under `cargo clippy`.
#[allow(dead_code)]
fn _hm() -> HashMap<String, Vec<(String, String)>> {
    HashMap::new()
}
