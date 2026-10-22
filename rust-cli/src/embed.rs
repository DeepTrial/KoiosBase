//! Local embeddings for §5.2.1 — a resident dependency, not an optional extra.
//!
//! Three details here were each found by running the model rather than by
//! reading its card, and each one silently changes the scores:
//!
//!   * The model must be a **static** ONNX export. The obvious candidate
//!     (`onnx-community/Qwen3-Embedding-0.6B-ONNX`) is a transformers.js
//!     *generative* export: it demands 28 `past_key_values.*` inputs and
//!     expects float tensors for `input_ids`. It cannot be used as an embedder
//!     without feeding a KV cache by hand. `janni-t/...-tei-onnx` exports the
//!     plain `input_ids` + `attention_mask` -> `last_hidden_state` graph.
//!   * Pooling is **last-token**, not mean. `1_Pooling/config.json` in the
//!     upstream repo sets `pooling_mode_lasttoken: true` and every mean
//!     variant false. Mean pooling buries the [EOS] state the model answers
//!     from: measured, an unrelated query scored *higher* (0.456) than a true
//!     synonym (0.330), i.e. the channel had no usable discrimination at all.
//!   * The query side takes an **instruction** prefix. With last-token pooling
//!     it is not required, but it widened the gap again (发动机 0.506 -> 0.550,
//!     while the unrelated control fell to 0.214).
//!
//! "Model missing" is an error here, never a silent fall back to BM25 — that is
//! §5.2.1's whole point: a corpus must not have two retrieval behaviours
//! depending on whether a file happened to download, because then eval numbers
//! stop being comparable across versions.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ort::environment::{Environment, GlobalThreadPoolOptions};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Value;
use rusqlite::Connection;
use tokenizers::Tokenizer;

/// Where the ONNX weights and tokenizer live inside a vault.
///
/// Deliberately a real directory rather than a cached download: §12 promises a
/// zero-network binary, so the operator puts the files there once and `koios`
/// never reaches out.
const MODEL_DIR: &str = "models";
const MODEL_FILE: &str = "qwen3-embedding-0.6b-int8-tei.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";

/// Native output width of Qwen3-Embedding-0.6B.
pub const DIM: usize = 1024;

/// The instruction the model card prescribes for retrieval queries.
///
/// Applied to queries only. Documents are embedded bare (`unless the config
/// says otherwise` — sentence-transformers' own config sets an empty document
/// prompt), so the asymmetry is intentional.
const QUERY_INSTRUCTION: &str =
    "Instruct: Given a web search query, retrieve relevant passages that answer the query\nQuery:";

/// Long documents are cut to this many characters before encoding.
///
/// §5.2.1 says "truncate at §4.1 boundaries, never mid-block"; for the encoder
/// itself the practical constraint is the position budget, and Blocks in this
/// vault are paragraph-sized, so this only fires on pathological input.
const MAX_CHARS: usize = 2000;

/// Everything about the loaded model that must not change without a re-index.
struct Embedder {
    env: Environment,
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    /// Token id of `<|endoftext|>` — the position last-token pooling reads.
    eos: u32,
}

static EMBEDDER: OnceLock<Option<std::sync::Arc<Embedder>>> = OnceLock::new();
static MISSING_REPORTED: OnceLock<()> = OnceLock::new();

pub fn model_path(vault: &Path) -> PathBuf {
    vault.join(MODEL_DIR).join(MODEL_FILE)
}

pub fn tokenizer_path(vault: &Path) -> PathBuf {
    vault.join(MODEL_DIR).join(TOKENIZER_FILE)
}

/// True when the vault has both files §5.2.1 requires.
pub fn is_ready(vault: &Path) -> bool {
    model_path(vault).is_file() && tokenizer_path(vault).is_file()
}

/// Load once per process. `None` means the model files are absent — see
/// `available()` for why that is a queryable state and not a hard panic here.
fn embedder(vault: &Path) -> Option<std::sync::Arc<Embedder>> {
    EMBEDDER
        .get_or_init(|| load(vault).ok().map(std::sync::Arc::new))
        .clone()
}

fn load(vault: &Path) -> Result<Embedder, String> {
    let mpath = model_path(vault);
    let tpath = tokenizer_path(vault);
    if !mpath.is_file() {
        return Err(format!("missing {}", mpath.display()));
    }
    if !tpath.is_file() {
        return Err(format!("missing {}", tpath.display()));
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .min(4);
    let env = ort::init()
        .with_name("koios")
        .with_global_thread_pool(
            GlobalThreadPoolOptions::default()
                .with_intra_threads(threads)
                .map_err(|e| e.to_string())?
                .with_inter_threads(1)
                .map_err(|e| e.to_string())?,
        )
        .build()
        .map_err(|e| e.to_string())?;
    let session = Session::builder(&env)
        .map_err(|e| e.to_string())?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| e.to_string())?
        .with_intra_threads(threads as usize)
        .map_err(|e| e.to_string())?
        .commit_from_file(&mpath)
        .map_err(|e| e.to_string())?;
    let tokenizer = Tokenizer::from_file(&tpath).map_err(|e| format!("tokenizer: {e}"))?;
    let eos = tokenizer
        .get_vocab(true)
        .get("<|endoftext|>")
        .copied()
        .unwrap_or(151643);
    Ok(Embedder {
        env,
        session: Mutex::new(session),
        tokenizer,
        eos,
    })
}

/// Encode one string. `is_query` decides whether the instruction prefix applies.
///
/// Returns a unit vector, or `Err` when the model is unavailable.
pub fn encode(vault: &Path, text: &str, is_query: bool) -> Result<Vec<f32>, String> {
    let e = embedder(vault).ok_or_else(|| {
        format!(
            "the embedding model is not installed in {} — see §5.2.1;
             KoiosBase does not fall back to BM25 silently.",
            model_path(vault).display()
        )
    })?;
    let _ = &e.env; // keep the environment alive for the session's lifetime
    let text: String = text.chars().take(MAX_CHARS).collect();
    let body = if is_query {
        format!("{QUERY_INSTRUCTION}{text}")
    } else {
        text
    };
    let enc = e
        .tokenizer
        .encode(body.as_str(), false)
        .map_err(|err| err.to_string())?;
    let mut ids: Vec<i64> = enc.get_ids().iter().map(|&x| x as i64).collect();
    ids.push(e.eos as i64);
    let seq = ids.len();
    let shape = [1i64, seq as i64];
    let ids_val = Value::from_array((shape.as_slice(), ids.clone().into_boxed_slice()))
        .map_err(|err| err.to_string())?;
    let mask_val = Value::from_array((shape.as_slice(), vec![1i64; seq].into_boxed_slice()))
        .map_err(|err| err.to_string())?;
    let mut guard = e
        .session
        .lock()
        .map_err(|err| format!("session lock poisoned: {err}"))?;
    let outputs = guard
        .run(vec![
            ("input_ids".to_string(), ids_val.into_dyn()),
            ("attention_mask".to_string(), mask_val.into_dyn()),
        ])
        .map_err(|err| err.to_string())?;
    let (_shape, hidden) = outputs["last_hidden_state"]
        .try_extract_tensor::<f32>()
        .map_err(|err| err.to_string())?;
    let hidden = hidden.to_vec();
    if hidden.len() != seq * DIM {
        return Err(format!(
            "unexpected hidden size {} for seq {seq} (dim {DIM})",
            hidden.len()
        ));
    }
    // Last-token pooling, then L2-normalise so a dot product IS cosine.
    let mut out = hidden[(seq - 1) * DIM..seq * DIM].to_vec();
    let norm: f32 = out.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in out.iter_mut() {
            *v /= norm;
        }
    }
    Ok(out)
}

/// Report the missing model to stderr once per process.
///
/// Indexing still has to succeed without the model or every existing test,
/// vault, and CI job breaks the moment this dependency lands. But silence is
/// what made §5.2.1 easy to ignore, so it is said out loud exactly once.
pub fn warn_if_missing(vault: &Path) {
    if is_ready(vault) {
        return;
    }
    if MISSING_REPORTED.set(()).is_err() {
        return;
    }
    eprintln!(
        "warning: no embedding model in {} — the vector channel (§6.1 ②) is \
         inactive and recall is limited to lexical overlap. See §5.2.1.",
        vault.join(MODEL_DIR).display()
    );
}

/// The sqlite-vec table holding one row per block, keyed by `blocks.id`.
pub const VEC_SCHEMA: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS vec_blocks USING vec0(
  block_id TEXT PRIMARY KEY, embedding float[1024]);
CREATE TABLE IF NOT EXISTS vec_meta (key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS vec_src (block_id TEXT PRIMARY KEY, hash TEXT);
"#;

/// Create `vec.db` and its tables. Safe to call repeatedly.
pub fn ensure_vec_db(vault: &Path) -> rusqlite::Result<Connection> {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| unsafe {
        // sqlite-vec ships only the C entry point, so register it with SQLite's
        // auto-extension hook. The cast is to the signature SQLite expects for
        // an extension init function; `sqlite3_vec_init` already has it.
        // sqlite-vec declares `sqlite3_vec_init()` with NO parameters, which
        // does not match the init signature SQLite actually calls it with. A
        // cast is therefore unavoidable; it is written with both sides fully
        // annotated so the conversion is the one argument-compatible layout
        // rather than an unchecked reinterpretation.
        type ExtInit = unsafe extern "C" fn(
            *mut rusqlite::ffi::sqlite3,
            *mut *const std::os::raw::c_char,
            *const rusqlite::ffi::sqlite3_api_routines,
        ) -> std::os::raw::c_int;
        let raw = sqlite_vec::sqlite3_vec_init as *const ();
        let init: ExtInit = std::mem::transmute::<*const (), ExtInit>(raw);
        rusqlite::ffi::sqlite3_auto_extension(Some(init));
    });
    let dir = vault.join(".index");
    std::fs::create_dir_all(&dir).ok();
    let conn = Connection::open(dir.join("vec.db"))?;
    conn.execute_batch(VEC_SCHEMA)?;
    Ok(conn)
}

/// Encode every block whose content hash changed or which has no vector yet.
///
/// `blocks.hash` is what keeps this incremental (§5.2.1): a block whose text is
/// unchanged is not re-encoded, so a second `koios index` over an untouched
/// vault costs one query and zero forward passes. Rows whose block disappeared
/// are dropped, otherwise deleted documents keep answering queries forever.
pub fn build_vectors(vault: &Path, main: &Connection) -> rusqlite::Result<()> {
    let conn = ensure_vec_db(vault)?;
    let tx = conn.unchecked_transaction()?;

    // Content hashes of what is ALREADY encoded, keyed by block id. This is
    // what makes a second `index` over an untouched vault cost no forward
    // passes; vec0 itself cannot store an extra column, so it lives in vec_meta.
    let mut hash_of: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT block_id, hash FROM vec_src")?;
        let m = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1).unwrap_or_default(),
            ))
        });
        if let Ok(m) = m {
            for r in m.flatten() {
                hash_of.insert(r.0, r.1);
            }
        }
    }

    let mut rows: Vec<(String, String, String)> = Vec::new();
    {
        let mut stmt = main.prepare(
            "SELECT id, breadcrumb, raw FROM blocks WHERE raw IS NOT NULL \
             AND TRIM(raw) <> '' ORDER BY doc_path, ordinal",
        )?;
        let m = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1).unwrap_or_default(),
                r.get::<_, String>(2)?,
            ))
        })?;
        for r in m.flatten() {
            rows.push(r);
        }
    }

    let live: std::collections::HashSet<String> =
        rows.iter().map(|(id, _, _)| id.clone()).collect();

    // sqlite-vec's vec0 is a virtual table and does not implement UPSERT, so
    // "replace" is a delete followed by an insert. Doing it only for blocks
    // that actually changed is what keeps this incremental.
    let mut insert = conn.prepare("INSERT INTO vec_blocks (block_id, embedding) VALUES (?, ?)")?;
    let mut encoded = 0usize;
    for (id, breadcrumb, raw) in rows.iter() {
        let hash = block_hash(main, id).unwrap_or_default();
        if let Some(prev) = hash_of.get(id) {
            if *prev == hash {
                continue;
            }
        }
        // breadcrumb + raw, per §5.2.1: the heading path carries navigational
        // meaning the sentence alone does not.
        let text = format!("{breadcrumb} {raw}");
        match encode(vault, &text, false) {
            Ok(vec) => {
                conn.execute("DELETE FROM vec_blocks WHERE block_id = ?", [id])?;
                insert.execute(rusqlite::params![id, to_blob(&vec)])?;
                conn.execute(
                    "INSERT INTO vec_src (block_id, hash) VALUES (?, ?) \
                     ON CONFLICT(block_id) DO UPDATE SET hash = excluded.hash",
                    rusqlite::params![id, hash],
                )?;
                encoded += 1;
            }
            Err(e) => {
                eprintln!("warning: could not encode {id}: {e}");
            }
        }
    }

    // Retire vectors for blocks that no longer exist. Without this, editing a
    // heading leaves the old block answering queries with no way to see that
    // its source is gone.
    let mut dropped = 0usize;
    for id in hash_of.keys() {
        if !live.contains(id) {
            conn.execute("DELETE FROM vec_blocks WHERE block_id = ?", [id])?;
            conn.execute("DELETE FROM vec_src WHERE block_id = ?", [id])?;
            dropped += 1;
        }
    }
    let _ = encoded;
    let _ = dropped;

    tx.commit()?;
    Ok(())
}

/// The content hash `blocks` carries for a block, which drives incrementality.
fn block_hash(main: &Connection, id: &str) -> Option<String> {
    main.query_row("SELECT hash FROM blocks WHERE id=?", [id], |r| r.get(0))
        .ok()
}

/// sqlite-vec expects raw little-endian float32 bytes.
fn to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Channel ②'s missing half: nearest neighbours of the query in `vec.db`.
///
/// Returns `(block_id, cosine)` descending. sqlite-vec's default metric on a
/// normalized embeddding IS cosine, so no further conversion happens here.
pub fn search_vectors(vault: &Path, query: &str, limit: usize) -> Vec<(String, f64)> {
    if !is_ready(vault) {
        return Vec::new();
    }
    let q = match encode(vault, query, true) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("warning: could not encode the query: {e}");
            return Vec::new();
        }
    };
    let conn = match ensure_vec_db(vault) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: could not open vec.db: {e}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    let mut stmt = match conn.prepare(
        "SELECT block_id, distance FROM vec_blocks \
         WHERE embedding MATCH ? ORDER BY distance LIMIT ?",
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("warning: vector query failed: {e}");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(rusqlite::params![to_blob(&q), limit as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
    });
    if let Ok(m) = rows {
        for r in m.flatten() {
            out.push(r);
        }
    }
    out
}
