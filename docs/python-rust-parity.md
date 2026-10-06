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

## eval 指标差分法（2026-10-02 新增）

同一 vault 上跑两壳的 `eval`，比对 `total=` 行。任何指标不同 = 行为缺口，不是噪声：

```bash
V=$(mktemp -d); rust-cli/target/release/koios init "$V"
printf -- '---\ntitle: Report\n---\n# 财务分析\n## 负债分析\n负债合计 18 亿元，营收 32 亿元。\n## 现金流\n经营性现金流为正。\n' > "$V/raw/report.md"
rust-cli/target/release/koios index "$V"
rust-cli/target/release/koios eval "$V" | grep ^total
PYTHONPATH=. .venv/bin/python -c "from koiosbase.cli import main; main(['eval','-p','$V'])" | grep ^total
```

预期两壳都是 `total=5 recall@1=0.600 refusal_acc=1.000 citation_cov=0.348`。

这个方法一次性挖出三个真实 Rust 缺口（均已修）：

1. coverage 测在 assembled context 上，而 Python 测 `res.answer`；
2. refusal case 没计入 coverage 总和（导致平均值虚高）；
3. `generate()` 输出了 `检索到的证据如下：` 前缀 + `(id)` 引用，而 Python
   是裸行 + `[[raw/{id}]]`。Python 特意删掉前缀 —— 没有引用的抬头句会让
   每个默认回答都违反 citation contract。每条都给所有分母多加一个句子。

**永远比对指标，不要只比对「有没有结果」。**

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

## ✅ Python 作为依赖已移除（2026-10-02，commit `b282366`）

上两节的两个 blocker 都已解除，Rust 现在已经自足且测试是超集：

| 指标 | 之前 | 现在 |
|---|---|---|
| Rust 测试数 | 18 | **87**（Python 是 65） |
| PDF 引擎 | pdf-extract（不同引擎） | **MuPDF** —— 与 PyMuPDF 同一个 |
| CLI 覆盖面 | 缺 3 项 | **21 个子命令**，拼写与 Python 一致 |
| 运行时依赖 | — | 除 cargo crate 外无；仅 5 个系统库 |

### 构建前置（缺了 bindgen 会失败）

```bash
sudo apt-get install -y clang libclang-dev pkg-config libc6-dev
export BINDGEN_EXTRA_CLANG_ARGS="-I/usr/lib/gcc/x86_64-linux-gnu/13/include"
cd rust-cli && cargo build --release
```

`src/pdf.rs` 的调用链是 `Document::open()` → `load_page(i)` →
`page.text(TextExtractOptions::default())` → `normalize_pymupdf_text()`。

### 这次踩过的坑（真金白银的时间）

- **`Page::text()` 不等于 PyMuPDF 的 `get_text()`** —— 它每行后会多出一个空行。
  必须归一化，否则所有派生索引都会错位，且 `page_to_blocks` 会变成一行一个 block。
- 换依赖后忘记更新，`Cargo.lock` 里仍留着 pdf-extract —— 用
  `grep -c pdf-extract Cargo.lock` 确认已清除。
- MuPDF 首次编译要几分钟，用后台终端跑。
- **`cargo fmt` 会重写测试文件** —— 跑完不要照着旧文本打补丁，先重读文件或直接用 patch 工具。

### 验证「真的不需要 Python 了」

```bash
mkdir -p /tmp/emptybin
for b in python python3; do printf '#!/bin/sh\nexit 127\n' > /tmp/emptybin/$b; chmod +x /tmp/emptybin/$b; done
export PATH=/tmp/emptybin:/usr/bin:/bin   # 保留 coreutils，只屏蔽 python
koios init . && koios index . && koios eval .
```

另外要比对**退出码**，不只是「有没有跑起来」：`lint` 和 `eval` 发现问题时故意返回非 0。
两边要用完全相同的参数各跑一次再 diff `$?`。

### 许可证提醒（必须随二进制分发）

`mupdf` crate 是 AGPL-3.0-or-later 或 Artifex 商业许可 —— 与原先 PyMuPDF 完全相同的条款。
义务种类没变，但现在挂在**分发的二进制**上，不再是解释器 import。
已记录在 `rust-cli/Cargo.toml`。任何人再分发此二进制需注意 AGPL 源码提供义务。
