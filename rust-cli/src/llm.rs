//! Model access: a config file, resolved once, used by every command.
//!
//! KoiosBase ships no vendor adapter and hardcodes no provider — what it does
//! ship is an **OpenAI-compatible client** and a small config file. Anything
//! that speaks that shape works: a hosted API, a local llama.cpp or vLLM
//! server, Ollama, a corporate proxy.
//!
//! ```toml
//! # <vault>/koios.toml
//! [model]
//! kind   = "chat"                       # chat | vision | command
//! base_url = "https://api.openai.com/v1"
//! model  = "gpt-4o-mini"
//! api_key_env = "OPENAI_API_KEY"        # name of the env var, never the key
//!
//! [model.vision]                        # optional; scanned PDF pages
//! model = "gpt-4o"
//!
//! [retrieval]
//! top = 8
//! ```
//!
//! Three things this deliberately does not do:
//!
//! - **Never store a key in the file.** `api_key_env` names an environment
//!   variable; the value is read at call time. A config file in a vault that
//!   people commit must not become a credential leak.
//! - **Never change the default.** With no `[model]` section — or no file at
//!   all — the built-in extractive generator is used, exactly as before, so
//!   `koios eval` and the test suite keep measuring the same thing.
//! - **Never invent.** A model that returns nothing, or errors, produces an
//!   error the caller sees. It does not fall back to made-up text.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Fields `ModelConfig` types itself; `flatten` would otherwise capture them
/// too and re-insert them over the typed values.
const KNOWN_MODEL_KEYS: &[&str] = &[
    "kind",
    "base_url",
    "model",
    "api_key_env",
    "vision",
    "command",
];

/// Where the config lives inside a vault.
pub const CONFIG_FILE: &str = "koios.toml";

/// Anything that speaks the OpenAI chat/vision HTTP shape.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ModelConfig {
    /// `chat` (default), `vision`, or `command` (escape hatch: run a program).
    pub kind: Option<String>,
    /// Base URL of an OpenAI-compatible endpoint, e.g.
    /// `http://localhost:11434/v1` for Ollama.
    pub base_url: Option<String>,
    pub model: Option<String>,
    /// Name of the environment variable holding the API key. The key itself is
    /// never written to the config file.
    pub api_key_env: Option<String>,
    /// Optional override for the vision tier (scanned PDF pages).
    pub vision: Option<VisionConfig>,
    /// Escape hatch: a program to run instead of an HTTP call. `chat` covers
    /// everything with an OpenAI-shaped endpoint; this is for the rest.
    pub command: Option<String>,
    /// Extra generation parameters (`temperature`, `max_tokens`, ...) written
    /// flat next to the others and forwarded to the provider verbatim.
    ///
    /// `flatten` rather than a nested `[model.options]` table so the obvious
    /// spelling works and is not silently dropped — an unknown key with plain
    /// serde would be ignored, and a user would wonder why `temperature = 0.2`
    /// had no effect.
    #[serde(flatten)]
    pub options: HashMap<String, toml::Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct VisionConfig {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct RetrievalConfig {
    pub top: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub model: Option<ModelConfig>,
    pub retrieval: Option<RetrievalConfig>,
}

/// Load `<vault>/koios.toml`. A missing file is not an error — it means
/// "no model configured", which is the default the whole test suite relies on.
pub fn load(vault: &Path) -> Result<Config, String> {
    let p = vault.join(CONFIG_FILE);
    if !p.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))
}

/// The `koios.toml` path for a vault, for error messages and `koios config`.
pub fn config_path(vault: &Path) -> PathBuf {
    vault.join(CONFIG_FILE)
}

/// Resolve `api_key_env` to the actual key. Missing variable is an error the
/// user can act on, not a silent anonymous request.
fn api_key(cfg: &ModelConfig) -> Result<String, String> {
    let Some(name) = cfg.api_key_env.as_deref() else {
        // Local servers (Ollama, llama.cpp) commonly need no key at all.
        return Ok(String::new());
    };
    std::env::var(name).map_err(|_| {
        format!("model config names api_key_env = \"{name}\" but that variable is not set")
    })
}

/// Is a chat model actually reachable? Drives whether `query` routes through it.
pub fn has_chat(cfg: &Config) -> bool {
    cfg.model
        .as_ref()
        .is_some_and(|m| m.model.is_some() || m.command.is_some())
}

/// Is a vision model actually reachable? Drives whether scanned pages get
/// transcribed or silently skipped.
pub fn has_vision(cfg: &Config) -> bool {
    cfg.model
        .as_ref()
        .and_then(|m| m.vision.as_ref())
        .is_some_and(|v| v.model.is_some())
        || cfg
            .model
            .as_ref()
            .and_then(|m| m.command.as_deref())
            .is_some_and(|c| !c.trim().is_empty())
}

/// One call to an OpenAI-compatible chat endpoint.
///
/// `system` is where the §6.5 contracts are stated to the model: cite every
/// factual sentence, and refuse when the evidence does not contain the answer.
/// The contract is still *checked* afterwards — prompting is not enforcement.
pub fn chat(cfg: &ModelConfig, question: &str, context: &str) -> Result<String, String> {
    let Some(model) = cfg.model.as_deref() else {
        return Err("config has no model.model".into());
    };

    // The escape hatch first: an explicit command beats any HTTP assumption.
    if let Some(cmd) = cfg.command.as_deref().filter(|c| !c.trim().is_empty()) {
        return run_command(
            cmd,
            context.as_bytes(),
            &[("KOIOS_MODE", "generate"), ("KOIOS_QUESTION", question)],
        );
    }

    let base = cfg
        .base_url
        .as_deref()
        .unwrap_or("https://api.openai.com/v1")
        .trim_end_matches('/');
    let key = api_key(cfg)?;

    let system = "You answer strictly from the EVIDENCE below. \
                  Rules: (1) every factual sentence must carry a citation in the \
                  form [[raw/block-id]] taken verbatim from the evidence; \
                  (2) if the evidence does not contain the answer, reply with \
                  exactly 资料中未涉及 and nothing else; (3) never add facts \
                  that are not in the evidence.";

    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": format!(
                "EVIDENCE:\n{context}\n\nQUESTION: {question}")}
        ],
    });
    if let Some(obj) = body.as_object_mut() {
        for (k, v) in &cfg.options {
            // Skip the fields we already know, so `flatten` cannot re-insert
            // them and clobber the typed values.
            if KNOWN_MODEL_KEYS.contains(&k.as_str()) {
                continue;
            }
            if let Ok(j) = serde_json::to_value(v) {
                obj.insert(k.clone(), j);
            }
        }
    }

    let mut req =
        ureq::post(&format!("{base}/chat/completions")).header("Content-Type", "application/json");
    if !key.is_empty() {
        req = req.header("Authorization", &format!("Bearer {key}"));
    }

    let resp = req
        .send_json(&body)
        .map_err(|e| format!("model request failed: {e}"))?;
    let json: serde_json::Value = resp
        .into_body()
        .read_json()
        .map_err(|e| format!("model returned unreadable JSON: {e}"))?;

    json.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            let err = json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown shape");
            format!("model response had no content: {err}")
        })
}

/// One call to an OpenAI-compatible vision endpoint for a rasterized page.
pub fn vision(cfg: &Config, png: &[u8]) -> Result<String, String> {
    let m = cfg
        .model
        .as_ref()
        .ok_or_else(|| "config has no [model] section".to_string())?;

    if let Some(cmd) = m.command.as_deref().filter(|c| !c.trim().is_empty()) {
        return run_command(cmd, png, &[("KOIOS_MODE", "vlm")]);
    }

    let v = m
        .vision
        .as_ref()
        .ok_or_else(|| "no [model.vision] configured for scanned pages".to_string())?;
    let Some(model) = v.model.as_deref() else {
        return Err("[model.vision] has no model".into());
    };
    let base = v
        .base_url
        .as_deref()
        .or(m.base_url.as_deref())
        .unwrap_or("https://api.openai.com/v1")
        .trim_end_matches('/');
    let key = api_key(&ModelConfig {
        api_key_env: v.api_key_env.clone().or_else(|| m.api_key_env.clone()),
        ..Default::default()
    })?;

    let b64 = base64(png);
    let body = serde_json::json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text":
                 "Transcribe this page exactly. Output only the text you can read."},
                {"type": "image_url", "image_url":
                 {"url": format!("data:image/png;base64,{b64}")}}
            ]
        }],
        "max_tokens": 2000,
    });

    let mut req =
        ureq::post(&format!("{base}/chat/completions")).header("Content-Type", "application/json");
    if !key.is_empty() {
        req = req.header("Authorization", &format!("Bearer {key}"));
    }
    let resp = req
        .send_json(&body)
        .map_err(|e| format!("vision request failed: {e}"))?;
    let json: serde_json::Value = resp
        .into_body()
        .read_json()
        .map_err(|e| format!("vision response unreadable: {e}"))?;
    json.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "vision response had no content".to_string())
}

/// Temporarily park SIGPIPE's disposition, restoring the *previous* handler on
/// drop rather than assuming one.
///
/// Needed because `main` puts SIGPIPE back to SIG_DFL (so `koios full | head`
/// exits quietly rather than panicking). Under SIG_DFL a `write` to a closed
/// pipe does NOT return `ErrorKind::BrokenPipe` — it raises the signal and the
/// process dies before the error can be observed. Writing the payload to a
/// model command that never reads stdin hit exactly that, so one broken page
/// killed the whole `index` run.
#[cfg(unix)]
struct SigpipeGuard(libc::sighandler_t);

#[cfg(unix)]
impl SigpipeGuard {
    unsafe fn ignoring() -> Self {
        Self(libc::signal(libc::SIGPIPE, libc::SIG_IGN))
    }
}

#[cfg(unix)]
impl Drop for SigpipeGuard {
    fn drop(&mut self) {
        unsafe { libc::signal(libc::SIGPIPE, self.0) };
    }
}

/// The escape hatch: run a program. stdin carries the payload, stdout the reply.
fn run_command(cmd: &str, stdin: &[u8], env: &[(&str, &str)]) -> Result<String, String> {
    use std::io::Write;
    let mut c = std::process::Command::new("sh");
    c.arg("-c").arg(cmd).envs(env.iter().copied());
    // The child inherits the parent's SIGPIPE disposition, and `main` sets it
    // to SIG_DFL so `koios full | head` exits quietly instead of panicking.
    // Handing that default to a user model command is wrong: many commands
    // legitimately finish by writing output the reader has stopped taking, and
    // under SIG_DFL they die mid-write — which is indistinguishable from a
    // broken model. Give the child back SIG_IGN across exec.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        c.pre_exec(|| {
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
            Ok(())
        });
    }
    let mut child = c
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn model command: {e}"))?;
    if let Some(mut s) = child.stdin.take() {
        // See `SigpipeGuard`: this write must not be able to kill us. A model
        // command that exits without reading its payload (`exit 7`, or any
        // mis-piped tool) closes the pipe under us, and that is the command's
        // verdict — `wait_with_output` reports it. One broken page must not
        // take the whole `index` run with it.
        #[cfg(unix)]
        let _guard = unsafe { SigpipeGuard::ignoring() };
        let _ = s.write_all(stdin);
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("model command failed: {e}"))?;
    if !out.status.success() {
        let e = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if e.is_empty() {
            format!("model command exited with {}", out.status)
        } else {
            e
        });
    }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if t.is_empty() {
        return Err("model command produced no output".to_string());
    }
    Ok(t)
}

/// Minimal base64. Avoids pulling a crate for one call site.
fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_means_no_model() {
        let v = std::env::temp_dir().join(format!("koios-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&v);
        std::fs::create_dir_all(&v).unwrap();
        let cfg = load(&v).unwrap();
        assert!(cfg.model.is_none());
        assert!(!has_chat(&cfg));
        assert!(!has_vision(&cfg));
    }

    #[test]
    fn parses_a_realistic_config() {
        let toml = r#"
[model]
kind = "chat"
base_url = "http://localhost:11434/v1"
model = "qwen2.5"
api_key_env = "MY_KEY"
temperature = 0.2

[model.vision]
model = "llava"

[retrieval]
top = 12
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        let m = cfg.model.unwrap();
        assert_eq!(m.model.as_deref(), Some("qwen2.5"));
        assert_eq!(m.api_key_env.as_deref(), Some("MY_KEY"));
        assert_eq!(m.vision.clone().unwrap().model.as_deref(), Some("llava"));
        assert_eq!(cfg.retrieval.unwrap().top, Some(12));
        assert!(has_chat(&Config {
            model: Some(m),
            ..Default::default()
        }));
        assert!(
            toml::from_str::<Config>(toml)
                .map(|c| has_vision(&c))
                .unwrap_or(false),
            "a [model.vision] model means the tier is live"
        );
    }

    #[test]
    fn a_key_is_a_variable_name_not_a_value() {
        // Guard the whole reason api_key_env exists: the file must never hold
        // the secret. If someone adds a plain `api_key` field this still
        // deserializes, but nothing reads it — assert we never do.
        let cfg: Config = toml::from_str("[model]\napi_key = \"sk-live-xxx\"\n").unwrap();
        let m = cfg.model.unwrap();
        assert!(
            m.api_key_env.is_none(),
            "a literal api_key must not be picked up"
        );
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn command_escape_hatch_receives_stdin() {
        let out = run_command("cat", b"payload", &[("KOIOS_MODE", "generate")]).unwrap();
        assert_eq!(out, "payload");
    }
}
