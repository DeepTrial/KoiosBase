//! Exercises the README's VLM snippet types against the real Mupdf-backed
//! rasterizer path. A stub callable stands in for a vision model so this runs
//! offline and proves the signature, not a particular provider.

use koios::pdf::extract_pages;
use std::path::Path;

fn vision_model_transcribe(_png: &[u8]) -> String {
    "transcribed page text".to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| "scan.pdf".into());
    let my_vlm =
        |_path: &str, png: &[u8]| -> Result<String, String> { Ok(vision_model_transcribe(png)) };
    let pages = extract_pages(Path::new(&path), Some(&my_vlm));
    println!("{} page(s) extracted", pages.len());
}
