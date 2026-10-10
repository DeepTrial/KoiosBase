//! MuPDF ↔ PyMuPDF extraction parity.
//!
//! Both shells now bind the SAME engine (MuPDF), so page text must come out
//! byte-identical. These expectations were CAPTURED FROM PyMuPDF by running
//! `pymupdf.open(f)[i].get_text()` on the fixtures below — they are not
//! hand-written guesses. If the engine is ever swapped again, this file is
//! what tells you the swap regressed.

use std::fs;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-mupdf-{}", name));
    fs::create_dir_all(&p).ok();
    p
}

/// A minimal one-page PDF built from raw operators — no external toolchain,
/// so this test cannot be invalidated by a missing library.
fn write_pdf(path: &std::path::Path, lines: &[&str]) {
    let mut content = String::from("BT /F1 12 Tf\n72 720 Td\n14 TL\n");
    for l in lines {
        let esc = l
            .replace('\\', r"\\")
            .replace('(', r"\(")
            .replace(')', r"\)");
        content.push_str(&format!("({esc}) Tj T*\n"));
    }
    content.push_str("ET\n");

    let objs = [
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
    out.push_str(&format!("xref\n0 {}\n", objs.len() + 1));
    out.push_str("0000000000 65535 f \n");
    for o in &offsets {
        out.push_str(&format!("{:010} 00000 n \n", o));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
        objs.len() + 1,
        xref
    ));
    fs::write(path, out).unwrap();
}

/// The whole point: no blank lines between content lines.
///
/// PyMuPDF's get_text() emits ONE newline per line. mupdf-rs `Page::text()`
/// emits a blank line after each, which would make every block's `raw` differ
/// from the Python shell and turn every paragraph into one-block-per-line.
#[test]
fn page_text_has_no_interleaved_blank_lines() {
    let d = fixture("blank");
    let p = d.join("t.pdf");
    write_pdf(&p, &["Alpha line.", "Beta line.", "Gamma line."]);
    let pages = koios::pdf::extract_pages_cpu(&p);
    assert_eq!(pages.len(), 1, "one page expected");
    let text = &pages[0];
    assert!(
        !text.contains("\n\n"),
        "no blank line may sit between content lines: {text:?}"
    );
    let n = text.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(n, 3, "three content lines expected, got {n}: {text:?}");
}

/// Every line ends with exactly one newline, like PyMuPDF.
#[test]
fn page_text_matches_pymupdf_shape() {
    let d = fixture("shape");
    let p = d.join("t.pdf");
    write_pdf(&p, &["First.", "Second."]);
    let pages = koios::pdf::extract_pages_cpu(&p);
    let text = &pages[0];
    assert!(text.ends_with('\n'), "must end with a newline: {text:?}");
    assert_eq!(
        text, "First.\nSecond.\n",
        "PyMuPDF would return exactly this shape"
    );
}

/// A corrupt/absent file degrades to "no pages" instead of aborting ingest.
#[test]
fn unreadable_pdf_degrades_to_no_pages() {
    let d = fixture("bad");
    let p = d.join("bad.pdf");
    fs::write(&p, b"not a pdf at all").unwrap();
    let pages = koios::pdf::extract_pages_cpu(&p);
    assert!(
        pages.is_empty(),
        "a corrupt PDF must degrade to zero pages, not panic"
    );
}

/// Multi-line pages still yield page-level provenance (§4.1): one entry per
/// page, so citations can name an exact page.
#[test]
fn blocks_carry_page_numbers() {
    let d = fixture("prov");
    let p = d.join("t.pdf");
    write_pdf(&p, &["Heading", "ACME revenue was 32 yuan in 2024."]);
    let pages = koios::pdf::extract_pages_cpu(&p);
    assert_eq!(pages.len(), 1);
    let blocks = koios::pdf::page_to_blocks("t.pdf", 1, &pages[0], 0);
    for b in &blocks {
        assert!(
            b.id.starts_with("t.pdf#p1/"),
            "every block id must carry its page: {}",
            b.id
        );
    }
}
