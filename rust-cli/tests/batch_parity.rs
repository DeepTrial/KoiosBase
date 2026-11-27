//! Batch encoding must produce vectors identical to per-row encoding.
//!
//! `encode_batch` buckets rows by exact token length, so every batch is
//! rectangular with zero padding -- required because this ONNX export lets
//! mask-0 pads leak into attention (measured: ~5% cosine drift with one pad
//! token). Rows with wildly different lengths are encoded both ways and must
//! match bitwise. Skipped when the model files are not present.

use std::path::PathBuf;

fn model_vault() -> Option<PathBuf> {
    let p = PathBuf::from("/tmp/kbmodels");
    if p.join("qwen3-embedding-0.6b-int8-tei.onnx").is_file() && p.join("tokenizer.json").is_file()
    {
        Some(p)
    } else {
        None
    }
}

#[test]
fn batch_matches_single_row_encode() {
    if !koios::embed::ort_available() {
        eprintln!("skip: binary built without ONNX Runtime");
        return;
    }
    let Some(v) = model_vault() else {
        eprintln!("skip: no model files");
        return;
    };
    // models_root() prefers KOIOS_MODELS_ROOT, so the vault path itself can
    // be a bare temp dir.
    std::env::set_var("KOIOS_MODELS_ROOT", &v);
    // Lengths chosen so padding differs per row, including a CJK row that
    // tokenizes very differently from the Latin ones.
    let texts = vec![
        "短句。".to_string(),
        "a much longer english sentence about turbocharged engines and revenue growth".to_string(),
        "涡轮增压引擎的热效率在高海拔环境下会显著下降，需要更复杂的控制系统。".to_string(),
        "x".to_string(),
    ];
    let batch = koios::embed::encode_batch(&v, &texts, false).expect("batch encode failed");
    assert_eq!(batch.len(), texts.len());
    for (i, t) in texts.iter().enumerate() {
        let single = koios::embed::encode(&v, t, false).expect("single encode failed");
        assert_eq!(
            single, batch[i],
            "row {i} differs: batch pooled a pad position instead of the last real token"
        );
    }
}
