//! §5.1 vision tier, driven through the CLI the way a user actually would:
//! `KOIOS_VLM_CMD` set, a scanned page in `raw/`, then `koios index`.
//!
//! A scanned PDF contributes **nothing** unless a vision command is configured.
//! That silence is the honest default, and it is easy to get wrong in the other
//! direction (inventing text for a page nobody read), so it is asserted here
//! rather than assumed.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-vlm-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(p.join("raw")).unwrap();
    p
}

/// A one-page PDF with no content stream — MuPDF extracts no text, so
/// `looks_scanned` is true and the vision tier is the only way in.
fn scanned_pdf() -> Vec<u8> {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!("xref\n0 {}\n", objs.len() + 1));
    out.push_str("0000000000 65535 f \n");
    for o in &offsets {
        out.push_str(&format!("{o:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objs.len() + 1
    ));
    out.into_bytes()
}

fn koios() -> Command {
    Command::new(env!("CARGO_BIN_EXE_koios"))
}

#[test]
fn scanned_page_is_silent_without_a_vision_command() {
    let v = vault("silent");
    fs::write(v.join("raw").join("scan.pdf"), scanned_pdf()).unwrap();

    let out = koios()
        .args(["index"])
        .arg(&v)
        .env_remove("KOIOS_VLM_CMD")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Nothing extracted, and — the point — nothing invented either.
    assert!(
        stdout.contains("indexed 0 blocks"),
        "a page with no text and no VLM must contribute nothing: {stdout}"
    );
}

#[test]
fn configured_vision_command_recovers_the_scanned_page() {
    let v = vault("recovered");
    fs::write(v.join("raw").join("scan.pdf"), scanned_pdf()).unwrap();

    let out = koios()
        .args(["index"])
        .arg(&v)
        // The stub ignores the PNG and prints fixed text; what matters is that
        // the tier ran at all, and that its output landed in the index.
        .env("KOIOS_VLM_CMD", "printf '%s' 'transcribed from the image'")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("indexed 0 blocks"),
        "vision tier should have produced a block: {stdout}"
    );

    let hits = koios()
        .args(["search", "transcribed", "-p"])
        .arg(&v)
        .output()
        .unwrap();
    let hits = String::from_utf8_lossy(&hits.stdout);
    assert!(
        hits.contains("transcribed from the image"),
        "recovered text must be searchable: {hits}"
    );
}

#[test]
fn a_failing_vision_command_does_not_abort_the_rest_of_the_document() {
    let v = vault("failing");
    fs::write(v.join("raw").join("scan.pdf"), scanned_pdf()).unwrap();
    // A readable Markdown doc alongside it: one bad page must not lose the rest.
    fs::write(
        v.join("raw").join("ok.md"),
        "---\ntitle: Fine\n---\n\n# Section\n\nPlain markdown survives.\n",
    )
    .unwrap();

    let out = koios()
        .args(["index"])
        .arg(&v)
        .env("KOIOS_VLM_CMD", "exit 7")
        .output()
        .unwrap();
    assert!(out.status.success(), "a broken VLM must not fail the build");

    let hits = koios()
        .args(["search", "survives", "-p"])
        .arg(&v)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&hits.stdout).contains("survives"),
        "the Markdown sibling must still be indexed"
    );
}
