# KoiosBase Python ↔ Rust 双壳对比台账

`rust-cli/` 是 `koiosbase/`（Python）的 Rust 重写。两边共用同一个 `.index/tree.db`
格式 —— 索引是派生的，但两个 shell 必须对"派生"的语义完全一致，否则一个写的库
另一个读不懂。

**最后同步/核对时间：2026-10-01，基于 commit `c89b37e` + 本次会话的补充。**

## 命名/位置映射表

| Python `koiosbase/` | Rust `rust-cli/src/` | 备注 |
|---|---|---|
| index/schema.py SCHEMA + state/model.py SCHEMA_EXTRA | lib.rs `SCHEMA` | 唯一一份定义（曾重复，已合并到 lib.rs） |
| index/schema.py `connect` | lib.rs `connect` | 打开 `<vault>/.index/tree.db` |
| parsers/markdown.py | lib.rs `index_file` / `split_frontmatter` / `fm_*` | Markdown 切块，块 id 必须逐字节相同 |
| parsers/pdf.py | pdf.rs + lib.rs `index_pdf` | PDF 适配器；CPU-only 文本层提取 |
| ingest/pipeline.py | ingest.rs + lib.rs `walk_*` / `cmd_index` | 两趟：先 raw，再生 sources/，再走 wiki |
| security/acl.py | acl.rs | `grants_for` / `allowed` / `filter_blocks` / `visible_paths` |
| state/model.py | state.rs | `set_block_state` / `cascade_retraction` / `filter_visible` / `disposition_*` |
| retrieval/router.py | retrieval.rs | BM25 + PPR + RRF |
| retrieval/channels.py | retrieval.rs `tree_search` / `full_corpus` + main.rs `channel_ids` | 本次补齐 `channel_ids` |
| query/pipeline.py | pipeline.rs | `retrieve_channel` / `full_query` / `assemble` / `grade` |
| generation/contracts.py | pipeline.rs `check_contracts` / `citation_coverage` / `REFUSAL` | |
| generation/judge.py | lint.rs `cross_judge` | 确定性占位实现（数字比较） |
| lint/gardener.py | lint.rs `run_l1` / `check_*` / `cmd_lint` | L1 程序化检查 + checkclaim |
| compile/*.py | compile.rs | entities / sources / gate / answer page |
| studio/exports.py | studio.rs | brief / mindmap |
| mcp/server.py | mcp.rs | JSON-RPC 2.0 over stdio，三个 tool |
| eval/harness.py | main.rs `cmd_eval` | 种子评测集 |
| cli.py | main.rs clap `Cmd` | |

## 已移植（功能对等）

| 命令 / 能力 | Python | Rust | 验证方式 |
|---|---|---|---|
| init | ✅ | ✅ | 目录 + AGENTS.md ⚠️ 见下方差异 |
| index | ✅ | ✅ | tests/ingest.rs：幂等 / 冷启动 == 热 / 派生页入索引 |
| search | ✅ | ✅ | 含 ACL + state + disposition 过滤 |
| search `-c/--channel` | ✅ | ✅ **本次补齐** | 实测 full/tree 两壳 id 一致 |
| tree / full / query | ✅ | ✅ | CLI 子命令 |
| eval | ✅ | ✅ | 种子集两边同源 |
| lint | ✅ | ✅ | L1 五项检查 |
| checkclaim | ✅ | ✅ | lint.rs cross_judge |
| retract | ✅ | ✅ | 本次确认：retract 后 search 不再返回该块 |
| compile | ✅ | ✅ | entities= sources= 输出同形状 |
| promote | ✅ | ✅ | confidence 状态机 |
| answer | ✅ | ✅ | 写回 wiki/answers/ |
| studio brief / mindmap | ✅ | ✅ | 含 ACL 过滤 |
| mcp (3 tools) | ✅ | ✅ | search / ask / write_answer |
| **§8.3 apply_disposition** | ✅ | ✅ **本次补齐** | 之前 Rust 读路径完全没有 |
| **MCP write 归属日志** | ✅ | ✅ **本次补齐** | `[koios-write]` 到 stderr |

## ⚠️ 已知不对等（未移植 / 差异）

1. **`init` 写的 AGENTS.md 内容不同**。Python（`ingest/pipeline.py cmd_init`）
   写完整四行维护契约；Rust `main.rs cmd_init` 只写一行
   `"# AGENTS.md\n\nKoiosBase maintenance contract.\n"`。
   [事实] 实测：同一空 vault 两壳 init 后 `wc -c AGENTS.md` 不同。
   影响：AGENTS.md 是 §4.4 写进 vault 的契约，短版丢失了三条规则。
   尚未修（低优先级，不影响索引/检索语义）。

2. **Python `--strict` eval 标志**：Python `koios eval --strict` 会把 needs-llm
   也算作失败；Rust `Cmd::Eval` 没有 `--strict`，固定宽松模式。
   两者都打印同样的 `total=/recall@1=/refusal_acc=/citation_cov=/needs_llm=` 行，
   但退出码语义不同（Python strict 下可能非 0）。

3. **`studio` 的 faq 模板**：Python `studio/exports.py` 有 `render_faq` +
   `type: faq` frontmatter，但 **两侧 CLI 都没有暴露 faq 子命令**（`choices=
   ["brief","mindmap"]`）。这是 Python 侧的死代码，Rust 没有移植它，
   不构成功能缺失。

4. **`--full`（index 强制全量重建）**：Python 有该 flag（当前实现里未实际改变
   行为），Rust `Cmd::Index` 无此参数。

5. **`cmd search` 输出格式不同**：Python 打印 `[i] id\nbreadcrumb\nsnippet`
   三行；Rust 打印 `[doc_path] id\nsnippet` 两行。**stdout 文本不完全逐字节相同**
   （已确认），但 id 集合和顺序一致。Python 的 test suite 不比对 stdout 格式，
   所以不影响 CI。

## 未计划移植（设计如此）

- `koiosbase/generation/judge.py` 的真实 cross-family judge（NLI 模型）——
  v0.1 两边都是确定性占位符，v0.3 再接。
- LLM 调用：两边都不做（generation 是模板填充）。

## 核对方法（复现）

```bash
# Rust
cd rust-cli && cargo build --release && cargo test --release
# Python
cd /home/ubuntu/dev/KoiosBase && .venv/bin/python -m pytest -q
# 交叉：同一 vault 分别用两壳 index + search，比对 block id 集合
```
