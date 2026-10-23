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
/// Where the ONNX weights and tokenizer may live.
///
/// Checked in order: `KOIOS_MODELS_ROOT` if set, else the vault's own
/// `models/`. A single model is ~600 MB, so requiring a copy per vault made
/// multi-project setups duplicate it; the shared root is the escape hatch.
const MODEL_DIR: &str = "models";
const MODEL_FILE: &str = "qwen3-embedding-0.6b-int8-tei.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";

/// Native output width of Qwen3-Embedding-0.6B.
pub const DIM: usize = 1024;

/// Instruction prefixes, chosen per query language.
///
/// Measured on zero-word-overlap queries (see §10.2): the English prompt is
/// best on 营业额 (0.544 vs 0.469 bare) and 雇员 (0.600 vs 0.541), the Chinese
/// one on 营业额 again (0.564) and is the only variant that beats bare on 发动机
/// (0.445 vs 0.375 with English). Neither wins outright, so the honest answer
/// is to match the query's language rather than hardcode one -- and all three
/// put the correct document first, so this is tuning, not a correctness fix.
///
/// Applied to queries only; documents stay bare, matching
/// sentence-transformers' own config in the upstream repo.
const QUERY_INSTRUCTION_EN: &str =
    "Instruct: Given a web search query, retrieve relevant passages that answer the query\nQuery:";
const QUERY_INSTRUCTION_ZH: &str = "指令：给定一个问题，检索能够回答该问题的相关段落\n查询：";

/// Pick the instruction matching the query's dominant script.
fn query_instruction(query: &str) -> &'static str {
    let cjk = query.chars().filter(|c| is_cjk_query(*c)).count();
    let latin = query.chars().filter(|c| c.is_ascii_alphabetic()).count();
    if cjk > latin {
        QUERY_INSTRUCTION_ZH
    } else {
        QUERY_INSTRUCTION_EN
    }
}

/// CJK ranges, kept local so `embed` does not depend on `retrieval`.
fn is_cjk_query(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3400}'..='\u{4dbf}').contains(&c)
}

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

static EMBEDDER: OnceLock<Result<std::sync::Arc<Embedder>, String>> = OnceLock::new();
static MISSING_REPORTED: OnceLock<()> = OnceLock::new();

thread_local! {
    /// Per-thread opt-out for running without embeddings.
    ///
    /// Thread-local, not process-global: `cargo test` runs a binary's tests in
    /// parallel, and with an AtomicBool whichever thread set it last decided
    /// whether *every* other test degraded silently. That made the fail-closed
    /// assertions racy -- one observed `true` that another test had set.
    static ALLOW_LEXICAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Let this thread index/search without embeddings (CLI: --allow-no-embedding).
///
/// Per-thread rather than per-process on purpose -- see ALLOW_LEXICAL.
pub fn set_allow_lexical(allow: bool) {
    ALLOW_LEXICAL.with(|c| c.set(allow));
}

pub fn allow_lexical() -> bool {
    ALLOW_LEXICAL.with(|c| c.get())
}

/// Model files: `KOIOS_MODELS_ROOT` if set, else the vault's own `models/`.
///
/// The env var is what makes one copy serve every vault. A model is ~600 MB,
/// so per-vault copies were not viable with more than one project.
fn models_root(vault: &Path) -> PathBuf {
    if let Ok(root) = std::env::var("KOIOS_MODELS_ROOT") {
        let p = PathBuf::from(root);
        if p.is_dir() {
            return p;
        }
        eprintln!(
            "warning: KOIOS_MODELS_ROOT is set to {} but that is not a directory; \
             falling back to <vault>/models",
            p.display()
        );
    }
    vault.join(MODEL_DIR)
}

pub fn model_path(vault: &Path) -> PathBuf {
    models_root(vault).join(MODEL_FILE)
}

pub fn tokenizer_path(vault: &Path) -> PathBuf {
    models_root(vault).join(TOKENIZER_FILE)
}

/// True when this binary was compiled with ONNX Runtime support.
///
/// Without this the no-ort build is indistinguishable from a build whose model
/// happens to be missing: both "work", and one of them silently answers
/// lexically against a corpus that has vectors. That is the two-behaviours
/// outcome §5.2.1 exists to prevent, reached through the build rather than the
/// filesystem.
pub const fn ort_available() -> bool {
    cfg!(any(feature = "ort-download", feature = "ort-system"))
}

/// True when the vault has both files §5.2.1 requires.
pub fn is_ready(vault: &Path) -> bool {
    ort_available() && model_path(vault).is_file() && tokenizer_path(vault).is_file()
}

/// Load once per process, keeping the failure so it can be reported.
///
/// Previously `.ok()` discarded the reason, so a real failure (an unsupported
/// session option, a corrupt file, a missing symbol) surfaced to the user as
/// "the embedding model is not installed", which sent them hunting for a file
/// that was there all along. The error text is now what reaches stderr.
fn embedder(vault: &Path) -> Result<std::sync::Arc<Embedder>, String> {
    EMBEDDER
        .get_or_init(|| load(vault).map(std::sync::Arc::new))
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
    let e = embedder(vault).map_err(|err| {
        format!(
            "the embedding model could not be used ({err}).\n  \
             model:      {}\n  tokenizer:  {}\n  \
             Install both files, or pass --allow-no-embedding to run lexically on purpose.",
            model_path(vault).display(),
            tokenizer_path(vault).display()
        )
    })?;
    let _ = &e.env; // keep the environment alive for the session's lifetime
    let text: String = text.chars().take(MAX_CHARS).collect();
    let body = if is_query {
        format!("{}{text}", query_instruction(&text))
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

/// Fail unless the embedding model is usable, or the caller opted out.
///
/// This is §5.2.1's "resident dependency, not an optional extra" made real.
/// It replaces a version that only printed a warning and carried on, which
/// meant the same corpus answered semantically on one machine and lexically
/// on another -- exactly the two-behaviours outcome the section exists to
/// prevent, and the opposite of what the section claimed happened.
pub fn require(vault: &Path) -> Result<(), String> {
    if allow_lexical() {
        if MISSING_REPORTED.set(()).is_ok() && !is_ready(vault) {
            eprintln!("note: running lexically (--allow-no-embedding); the vector channel is off.");
        }
        return Ok(());
    }
    if !ort_available() {
        return Err(
            "this binary was built without ONNX Runtime, so it cannot embed.\n  \
             Rebuild with --features ort-download (bundled runtime, needs a newer\n  \
             glibc) or --features ort-system (links the system runtime).\n  \
             Or pass --allow-no-embedding to accept lexical-only retrieval."
                .to_string(),
        );
    }
    if !is_ready(vault) {
        return Err(format!(
            "no embedding model installed.\n  expected: {}\n            {}\n  \
             §5.2.1 makes embeddings a resident dependency: without them this\n  \
             vault would answer one way here and another way elsewhere.\n  \
             Install the two files, or pass --allow-no-embedding to accept that.",
            model_path(vault).display(),
            tokenizer_path(vault).display()
        ));
    }
    // Files exist; prove they actually load. Reporting "not installed" for a
    // file that IS installed was the old failure mode, hence the encode path
    // carrying the real reason.
    embedder(vault).map(|_| ())
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

/// Identity of the model an index was built with (§5.2.1 model lock).
///
/// Without this nothing could tell that `vec.db` was produced by a different
/// model, dimension, or pooling rule than the one now being queried. Vectors
/// from two models are not comparable distances, so silently mixing them
/// returns plausible-looking nonsense rather than nothing.
const POOLING: &str = "last-token+l2";
const MODEL_ID: &str = "qwen3-embedding-0.6b-int8-tei";

/// How a block's text is assembled before encoding.
///
/// Versioned because changing it silently invalidates every stored vector
/// while leaving `model_id` and `dim` untouched -- which is exactly what
/// happened when the join changed from a space to a newline: the index still
/// claimed to be current, and nothing could tell that half of it had been
/// encoded from a different text than the other half. Distances between
/// vectors built from two recipes are not comparable.
pub const EMBED_RECIPE: &str = "last-heading:nl:body:v3";

fn expected_meta() -> Vec<(&'static str, String)> {
    vec![
        ("model_id", MODEL_ID.to_string()),
        ("dim", DIM.to_string()),
        ("pooling", POOLING.to_string()),
        ("normalize", "l2".to_string()),
        ("embed_recipe", EMBED_RECIPE.to_string()),
    ]
}

/// A machine-readable description of the vector channel's state.
///
/// Callers previously had no way to ask: errors arrive as free text, so a
/// client could not distinguish "no model installed" from "this index was
/// built by a different model" without string matching. Every field here is
/// something the operator otherwise has to infer from stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// True when both model files that §5.2.1 requires are present.
    pub model_installed: bool,
    /// False when this binary was built without ONNX Runtime support.
    pub ort_available: bool,
    /// Set when the caller opted out of embeddings for this thread.
    pub lexical_only: bool,
    /// True when this install can actually retrieve semantically.
    pub vector_channel_active: bool,
    pub model_id: &'static str,
    pub dim: usize,
    pub pooling: &'static str,
    /// The identity recorded in `vec.db`, if one has been built yet.
    pub index_model_id: Option<String>,
    pub index_dim: Option<String>,
    /// True when the index disagrees with this build's model.
    pub index_mismatch: bool,
    /// Why the model could not be used, when that happened.
    pub error: Option<String>,
}

/// Report the vector channel's state without side effects on retrieval.
///
/// Deliberately does not attempt to load the model; that is an expensive
/// operation and `is_ready` plus the recorded error is enough to tell the
/// caller why semantic recall is off. `error` carries the last failure seen
/// during this process, if there was one.
pub fn status(vault: &Path) -> Status {
    let installed = is_ready(vault);
    let lexical = allow_lexical();
    let mut out = Status {
        model_installed: installed,
        ort_available: ort_available(),
        lexical_only: lexical,
        vector_channel_active: installed && !lexical,
        model_id: MODEL_ID,
        dim: DIM,
        pooling: POOLING,
        index_model_id: None,
        index_dim: None,
        index_mismatch: false,
        error: None,
    };
    // The early return is deliberately only about the model, not the index:
    // even a caller running lexically still needs to know that the stored
    // vectors came from a different model, since a later non-lexical process
    // would refuse them.
    if lexical {
        read_index_state(vault, &mut out);
        return out;
    }
    read_index_state(vault, &mut out);
    if !installed {
        out.error = require(vault).err();
    }
    out
}

/// Fill in what `vec.db` records about itself.
fn read_index_state(vault: &Path, out: &mut Status) {
    match ensure_vec_db(vault) {
        Ok(conn) => {
            let mut seen = std::collections::HashMap::new();
            if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM vec_meta") {
                if let Ok(rows) = stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1).unwrap_or_default(),
                    ))
                }) {
                    for r in rows.flatten() {
                        seen.insert(r.0, r.1);
                    }
                }
            }
            out.index_model_id = seen.get("model_id").cloned();
            out.index_dim = seen.get("dim").cloned();
            // A mismatch is what check_identity rejects during retrieval, so
            // surface the same judgement the retrieval path will apply.
            if !seen.is_empty() {
                out.index_mismatch = check_identity(&conn).is_err();
            }
        }
        Err(e) => out.error = Some(format!("could not read vec.db: {e}")),
    }
}

/// Write the current identity into `vec_meta`. Exposed so tests can build an
/// index that claims a specific recipe without running real inference.
pub fn stamp_identity_for_test(conn: &Connection) -> rusqlite::Result<()> {
    stamp_identity(conn)
}

/// Validate `vec_meta` against this build. Exposed for tests.
pub fn check_identity_for_test(conn: &Connection) -> Result<(), String> {
    check_identity(conn)
}

/// Reject a `vec.db` built by anything other than the current model.
fn check_identity(conn: &Connection) -> Result<(), String> {
    let mut have = std::collections::HashMap::new();
    let mut stmt = conn
        .prepare("SELECT key, value FROM vec_meta")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1).unwrap_or_default(),
            ))
        })
        .map_err(|e| e.to_string())?;
    for r in rows.flatten() {
        have.insert(r.0, r.1);
    }
    if have.is_empty() {
        // Nothing encoded yet, so nothing to disagree with.
        return Ok(());
    }
    for (k, v) in expected_meta() {
        match have.get(k) {
            Some(found) if found == &v => {}
            other => {
                return Err(format!(
                    "this index was built with {k}={}, but this build uses {k}={v}.\n  \
                     The two are not comparable distances; rerun `koios index --full`.",
                    other.map(|s| s.as_str()).unwrap_or("<unset>")
                ));
            }
        }
    }
    Ok(())
}

/// Record (and enforce) which model this index belongs to.
fn stamp_identity(conn: &Connection) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO vec_meta (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?;
    for (k, v) in expected_meta() {
        stmt.execute(rusqlite::params![k, v])?;
    }
    Ok(())
}

/// Drop every stored vector so the next `index` rebuilds them all.
///
/// Needed by `--full`. The incremental path skips blocks whose hash is
/// unchanged, which is right day to day, but it means "full rebuild" has to
/// mean something stronger than "run index again" -- otherwise vectors
/// encoded under an older model or recipe survive a rebuild that claims to be
/// complete.
pub fn reset_vectors(vault: &Path) -> rusqlite::Result<()> {
    let conn = ensure_vec_db(vault)?;
    let tx = conn.unchecked_transaction()?;
    conn.execute("DELETE FROM vec_blocks", [])?;
    conn.execute("DELETE FROM vec_src", [])?;
    conn.execute("DELETE FROM vec_meta", [])?;
    tx.commit()?;
    eprintln!("[index] --full: dropped stored vectors; re-encoding all");
    Ok(())
}

/// `blocks.hash` is what keeps this incremental (§5.2.1): a block whose text is
/// unchanged is not re-encoded, so a second `koios index` over an untouched
/// vault costs one query and zero forward passes. Rows whose block disappeared
/// are dropped, otherwise deleted documents keep answering queries forever.
pub fn build_vectors(vault: &Path, main: &Connection) -> rusqlite::Result<()> {
    let conn = ensure_vec_db(vault)?;
    // Refuse to extend an index built by a different model rather than let two
    // embedding spaces share one distance comparison (§5.2.1 model lock).
    check_identity(&conn).map_err(rusqlite::Error::InvalidParameterName)?;
    stamp_identity(&conn)?;
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
    let mut failed = 0usize;
    for (id, breadcrumb, raw) in rows.iter() {
        let hash = block_hash(main, id).unwrap_or_default();
        if let Some(prev) = hash_of.get(id) {
            if *prev == hash {
                continue;
            }
        }
        // breadcrumb + raw, per §5.2.1: the heading path carries navigational
        // meaning the sentence alone does not.
        // A/B measured on 7 MIRACL-zh queries (mean cosine, best-of count):
        //   full path `doc.md > 标题\n正文`   0.7298  best 1/7
        //   last heading `标题\n正文`         0.7373  best 5/7
        //   body only                        0.6838  best 1/7
        // The document filename contributes nothing and dilutes the heading.
        // Keep the last path segment only.
        let heading = breadcrumb
            .rsplit(" > ")
            .next()
            .unwrap_or(breadcrumb.as_str());
        let text = format!("{heading}\n{raw}");
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
                failed += 1;
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

    tx.commit()?;

    // #3 — report what happened, and fail when the index is incomplete. These
    // counts used to be computed and then discarded, and a block that failed
    // to encode only warned while `index` still exited 0, so a half-built
    // vector index was indistinguishable from a complete one from outside.
    if encoded > 0 || dropped > 0 || failed > 0 {
        println!("vectors: encoded={encoded} dropped={dropped} failed={failed}");
    }
    if failed > 0 {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "{failed} block(s) could not be encoded; the vector index is \
             incomplete. Fix the cause above, or run with \
             --allow-no-embedding to index lexically on purpose."
        )));
    }
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
pub fn search_vectors(
    vault: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<(String, f64)>, String> {
    if allow_lexical() || !is_ready(vault) {
        return Ok(Vec::new());
    }
    require(vault)?;
    let q = encode(vault, query, true)?;
    let conn = ensure_vec_db(vault).map_err(|e| format!("could not open vec.db: {e}"))?;
    check_identity(&conn)?;
    let mut out = Vec::new();
    let mut stmt = conn
        .prepare(
            "SELECT block_id, distance FROM vec_blocks \
             WHERE embedding MATCH ? ORDER BY distance LIMIT ?",
        )
        .map_err(|e| format!("vector query failed: {e}"))?;
    let rows = stmt.query_map(rusqlite::params![to_blob(&q), limit as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
    });
    if let Ok(m) = rows {
        for r in m.flatten() {
            out.push(r);
        }
    }
    Ok(out)
}
