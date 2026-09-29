# koios (Rust CLI spike)

Feasibility spike: can a compiled shell drive the SAME KoiosBase vault format as
the Python implementation? Scope limited to `init` / `index` / `search`.

## Verified
- **Format compatibility both ways.** A vault indexed by this binary is read
  back by the Python side (`blocks` + FTS rows intact), and this binary searches
  a vault indexed by Python. The vault format is therefore not Python-owned.
- **Chinese BM25 works.** SQLite `unicode61` treats a contiguous CJK run as one
  token, so `营收` never matches. The fix is identical to the Python side: store
  BOTH the original text and a per-character padded form, and pad the query too.
- **Single-file distribution.** `cargo build --release` yields one 3.1 MB
  dynamically linked binary (5 shared libs, all standard). No runtime, no venv.

## Measured (3000 docs / 3000 blocks, this machine)
| | Rust | Python |
|---|---|---|
| index | **0.74 s** | 1.56 s |
| 20x query | **0.08 s** | 0.18 s |

Caveat: the first Rust run was **24.7 s** — slower than Python — purely because
each INSERT was its own implicit transaction and fsync. Wrapping ingest in one
transaction gave the 0.74 s above. Language was never the bottleneck; the
transaction boundary was.

## Not implemented (spike scope only)
PDF adapter, wiki/compile layer, ACL, MCP, Studio, gardener, eval. Those remain
Python-side; this spike does not replace the Python CLI.
