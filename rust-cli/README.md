# koios (Rust shell)

KoiosBase is a single Rust binary. This directory is its whole implementation:
CLI entry point (`src/main.rs`) plus a library facade (`src/lib.rs`) so the
integration tests can drive the modules directly.

```bash
sudo apt-get install -y clang libclang-dev      # mupdf-sys builds via bindgen
cargo build --release --manifest-path Cargo.toml
./target/release/koios --help
```

16 subcommands (`--help` for the full list), same spelling as the Python CLI it
replaced — including the one-word `checkclaim`, which clap would otherwise
rename to `check-claim` and silently break scripts ported from Python.

## Where things live

| File | Responsibility |
| --- | --- |
| `src/lib.rs` | block model, parser, indexer, `connect()`, `SCHEMA` |
| `src/ingest.rs` | `raw/` + `wiki/` → derived index |
| `src/pdf.rs` | MuPDF-backed PDF adapter (CPU tier + VLM tier) |
| `src/retrieval.rs` | BM25, RRF fusion, personalized PageRank, channel ①/④ |
| `src/pipeline.rs` | retrieve → assemble → generate, §6.5 contracts |
| `src/compile.rs` | entity + sources pages, quality gate, synthesis layer |
| `src/state.rs` | knowledge state machine and retraction cascade |
| `src/lint.rs` | L1 gardener, `checkclaim` |
| `src/acl.rs` | ACL / multi-tenancy |
| `src/mcp.rs` | JSON-RPC 2.0 over stdio, three tools |
| `src/studio.rs` | brief / mindmap / FAQ exports |
| `src/main.rs` | clap CLI surface |

## Tests

```bash
cargo test --release --manifest-path Cargo.toml    # 122 tests, 17 suites
cargo fmt  --manifest-path Cargo.toml --all -- --check
cargo clippy --release --manifest-path Cargo.toml -- -D warnings
```

The suites are named after what they guard, not after the modules: `parity.rs`
(index db equality against the retired Python implementation),
`synthesis_parity.rs` (lazy-recompile lifecycle), `acl_security.rs`
(restricted blocks never reach an anonymous caller), `search_stdout.rs`
(three-line hit format), and so on. Their assertions encode the Python
implementation's observable behaviour, which is why they survive its removal —
see `docs/python-rust-parity.md` for how those values were obtained.

## Examples

`examples/readme_llm_snippet.rs` and `examples/readme_vlm_snippet.rs` are the
callable-API examples quoted in the top-level README. They compile and run:

```bash
cargo run --release --example readme_llm_snippet -- path/to/vault
cargo run --release --example readme_vlm_snippet -- path/to/scan.pdf
```

## Dependencies

Deliberately small — everything needed is either compiled in or a system
library already present.

| Crate | Purpose |
| --- | --- |
| `rusqlite` (`bundled`) | SQLite **with FTS5** compiled from source |
| `mupdf` | PDF extraction and page rasterization |
| `clap` | CLI parsing |
| `regex` | lot code paths (CJK padding, `checkclaim`) |
| `serde_json` (`preserve_order`) | MCP payloads, key order matters |
| `sha2` | content hashes for incremental rebuilds |

## License caveat worth repeating

The `mupdf` crate is **AGPL-3.0-or-later or Artifex commercial**. KoiosBase
itself is MIT, but distributing this binary attaches that obligation to you.
It is the same position the retired Python shell was in (PyMuPDF carried the
identical terms); what changed is that the obligation now sits on a distributed
binary rather than an interpreter-time import. See the note in `Cargo.toml`.
