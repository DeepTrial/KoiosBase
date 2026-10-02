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

## 能否删掉 Python、只留 Rust？（2026-10-02 实证结论）

commit `505647a` 时的实测数据：

| 指标 | Python | Rust |
|---|---|---|
| 实现 LOC | 4194 | 4155 |
| 测试数 | 65 | 18 |
| 运行时依赖 | pymupdf（**65 MB**） | 无（仅 cargo crate） |
| 产物 | wheel，需解释器 | **8.3 MB** 静态二进制，已构建 3 个交叉目标 |

**结论：不建议删，且原因不是「Rust 没写完」。**

** blocker 1：PDF 引擎不是同一个东西。**
Python 用 PyMuPDF（MuPDF 的 C++ binding，65 MB，成熟）；Rust 用 `pdf-extract`
（纯 Rust，小得多）。这是**两个不同的 PDF 引擎**。在格式良好的文本型 PDF 上，
实测两壳抽出的 block **逐字段完全一致**（含 type/breadcrumb/raw）。但边角情况会分叉：
一页若每行都短且无句末标点，两壳共用的 heading 启发式会把正文误判成标题、
**产出 0 个 block** —— 这是两壳**共有**的潜在 bug（同一算法的遗传），
不是移植遗漏。换引擎意味着要在扫描件/图片型 PDF、表格、CJK 版式上重新建立信任，
而这些恰恰两边测试都没覆盖。

** blocker 2：Python 测试是 oracle。**
Rust 的 18 个测试是对着它做 parity 断言的。删掉之后这些测试变成自证 ——
断言「Rust 今天做什么」，而不是「应该是什么」。本轮修的 MCP 通知 bug、
§8.3 disposition 缺口，**全部是靠跟 Python 比对才发现的**，Rust 自己的测试一个都没报。

**若仍要推进，安全顺序是：**
1. 先把 `pdf-extract` 换成 MuPDF 系 crate（`mupdf` crate），让两边的提取引擎变成同一个，
   再重跑 PDF 差分测试。
2. 把 Rust 测试从 18 补向 Python 的 65 个用例（用 Python 输出做 snapshot 基准），
   **补完再删**，而不是反过来。
3. Python 包可以停止加新功能（冻结），Rust 二进制作为默认 `koios` 发布；
   **删源码是最后一步**。

在 1、2 完成前不要删。
