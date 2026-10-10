# KoiosBase Python ↔ Rust 双壳对比台账

> **状态（2026-10-08）：Python 侧已从仓库移除**（commit `c922be4` 起陆续删
> `koiosbase/`、`tests/`、`tools/`，并上线 Rust-only CI）。本文件保留为**移植
> 审计轨迹**：下面的差分方法与结论全部是 2026-10-06/10-08 实跑得出的。
> 要在昔日形式手工复跑某一节，用 `git show <commit>:path` 取回当时的 Python
> 源文件即可（例如 `git show cddcb64:koiosbase/cli.py`）。

`rust-cli/` 曾是 `koiosbase/`（Python）的 Rust 重写。两边共用同一个
`.index/tree.db` 格式 —— 索引是派生的，但两个 shell 必须对"派生"的语义完全一致，
否则一个写的库另一个读不懂。

**最后同步/核对时间：2026-10-06，基于 commit `a8195be`。**

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

1. ~~**`init` 写的 AGENTS.md 内容不同**~~ ✅ **已于 2026-10-06 修**。
   [实验] 同一空 vault 两壳 init 后 `diff AGENTS.md` 无输出（逐字节相同）。
   加锁：`tests/parity.rs::init_writes_the_full_maintenance_contract`。

2. ~~**Python `--strict` eval 标志**~~ ✅ **已补齐**。Rust `Cmd::Eval` 现支持
   `--strict`。[实验] 同一 vault 上
   `koios eval --strict` 与 `python -m koiosbase.cli eval -p V --strict`
   均输出 `total=5 recall@1=0.600 refusal_acc=1.000 citation_cov=0.348 needs_llm=0`
   且退出码同为 0。

3. **`studio` 的 faq 模板**：Python `studio/exports.py` 有 `render_faq` +
   `type: faq` frontmatter，但 **两侧 CLI 都没有暴露 faq 子命令**（`choices=
   ["brief","mindmap"]`）。这是 Python 侧的死代码，Rust 没有移植它，
   不构成功能缺失。

4. ~~**`--full`（index 强制全量重建）**~~ ✅ **已补齐**。Rust `Cmd::Index` 现接受
   `--full` 且与 Python 同为 no-op（content hash 已让每次重建都正确）。
   [实验] `koios index --full` 输出 `[index] --full accepted; rebuild is
   already complete` 后正常索引。保留该 flag 只为脚本可移植。

5. ~~**`cmd search` 输出格式不同**~~ ✅ **已于 2026-10-06 修**。
   Rust 原先打印 `[doc_path] id\n  snippet` 两行，丢掉了 breadcrumb；
   现改为与 Python 逐字节相同的三行：
   `[i] id` / 缩进 breadcrumb / 缩进 snippet（snippet 内换行也同 Python 一样
   折成空格）。
   [实验] `diff` 两壳 stdout 无差异。
   加锁：`tests/search_stdout.rs`（两个用例：三行结构 + `(no hits)` 哨兵）。

## 本轮（2026-10-06）由 stdout 差分挖出的根因式缺口

上面第 5 条只是表象。修完后对 `lint` 做差分，又暴露出两个躲在索引层的真差异 ——
它们只影响写盘后由下游命令读取的形状，单元测试（测 helper 本身）全是绿的：

6. **`blocks.raw` 丢掉了块内换行**。Rust `lib::index_file` 的 `flush()` 用
   `" "` join 行，Python `parsers/markdown.py split_blocks()` 用 `"\n"`。
   多行列表被压成一行 → lint 按行走的 `unsourced_assertions` 报 1 条而 Python
   报 3 条，所有派生页的 raw 也不同。
   [实验] 修后同一 vault 两壳 `lint` stdout `diff` 无差异（10 finding 全同）。
   加锁：`tests/lint_parity.rs::indexed_blocks_keep_their_interior_newlines`。

7. **`check_orphan_pages` 只看出边**。Rust 原先仅检查块内有没有 `[[`，于是
   **每个 Hub 页都被当成垃圾报出来**——最值得留的页恰恰是"被很多人引用、
   自己不引用任何人"。Python 早年为这个理由修过，Rust 一直没跟上。
   [实验] 现同 Python 一致：无引用时报 `orphan page (no links in or out)`，
   有一条入边后即不再报。
   加锁：`tests/lint_parity.rs::orphan_check_honours_inbound_citations`。

> 教训（同 eval 差分法）：**永远 diff 两壳的 stdout，而不是只读代码找差异。**
> 这两个缺口都藏在索引写盘的形状里，`cargo test` 全绿也照样不对等。

> Rust 编译现为 **零警告**（清理了 3 个 unused import 与 `mcp::_unused` 死代码）。
> `cargo test` **18 组全通过**（Python 侧 65 个 pytest 亦全通过）。

## 全表 DB 差分法（2026-10-06 新增，比 stdout 差分更狠）

stdout 差分受 CLI 命令覆盖面限制。更强的做法：同一 vault 分别用两壳 `index`，
然后逐表逐行比对：

```bash
V=$(mktemp -d); V2=$(mktemp -d)
for v in "$V" "$V2"; do koios init "$v"; # 放同样的 raw/ 内容
done
rust-cli/target/release/koios index "$V"
PYTHONPATH=. .venv/bin/python -c "from koiosbase.cli import main; main(['index','$V2'])"
# 然后 SELECT 每张表 ORDER BY id，逐元组比对
```

[实验] 现对 blocks / sections / documents / links 四张表达成 **全行相同**，
语料覆盖 Markdown、受限 ACL 文档、PDF，以及混合 PDF+MD。

这一路又挖出四个只有 DB 比对才能看见的缺口（均已修）：

8. **`sections.layer` 恒为 `'raw'`**。`index_file` 的 INSERT 把 layer 硬编码，
   丢了入参，于是派生页的 sections 全被写成 raw 层 —— 所有 `layer='wiki'`
   的检查（含上面的孤儿检查）直接跳过它们。
9. **缺少 `list` 块类型**。Python `split_blocks` 会跟踪到 run 结束给出
   `type='list'`；Rust 只分 paragraph/table，每个 `- item` 块都被标成
   `'paragraph'`。
10. **`sections.summary` 保留了多行原始形态**。Python 回填算的是
    `own[0].replace("\n"," ")[:160]`；Rust 直接塞 raw。
11. **`documents.frontmatter` 的 JSON 格式不同**。Python 用 `json.dumps()`
    默认分隔符（`", "` / `": "`），serde_json 默认无空格。新增
    `lib::json_python_dumps` 复现该格式（键序靠 serde_json 的 `preserve_order`
    feature 保持，与 Python dict 一致）。

以及 PDF-only 的两个：

12. **`collect_raw_docs` 只收 `.md`** → raw/ 里的 PDF 有 blocks 却没有
    sources/ 导航页（§4.3）。Python 走 `_read_raw_docs` → `parse_any`，收所有格式。
    [实验] 修前同一 PDF vault：Rust 1 block vs Python 5；修后全表相同。
    新增依赖 `sha2`（用于 doc_hash）。
13. **PDF frontmatter 缺 `doc_hash`、且 `ocr` 是字符串不是布尔**。
    [实验] 修后 `{"title":..., "parser":"pdf-cpu", "doc_hash":"sha256:...",
    "ocr": false}` 与 Python 逐字节一致。
    加锁：`tests/pdf_source_parity.rs`（3 个用例）。

## Python-only 库能力的迁移（2026-10-08，commit `c922be4` / `46d130e` / `cddcb64`）

前面的差分能保证"已实现的部分逐字节一致"，但保证不了"没漏实现"。对这两个
_once cone也只有 Python 才有的能力_做了专项核查——`rust-cli/src/*.rs` 里
`Fn`/`impl Fn`/`dyn Fn` 曾**零命中**：

14. **`query(..., llm=callable)`**（README §237 承诺）。Rust `full_query` 无此参数。
    新增 `full_query_with(conn, q, top, groups, Option<&dyn Fn>)`，且回调产出
    **同样过 `check_contracts` 闸门**（§6.5：给 KoiosBase 一个模型，不等于让它编造）。
    `QueryResult` 同时补齐 `violations` 与 `trace`（§6.2 Self-Route 面包屑）。
15. **`parse_pdf(..., vlm=callable)`**（README §369 承诺，扫描件 OCR）：
    `pdf::extract_pages(path, vlm)` 实现 §5.1 两档-tier（CPU 无文本→PNG 交给 VLM），
    并补上 `load_cache`/`save_cache` 解析缓存。
16. **`_migrate()` 旧库迁移**：Rust 无 ALTER 路径，故 layer/ordinal 之前的 vault
    在 Rust 侧直接打不开。现 `connect()` 自动补列。
17. **`build_page_confidence` + 加权图**：Rust `hybrid_search` 用的是**未加权**图，
    导致 §6.6 权威阶梯是死代码——草稿页的引用和人肉核验过的权重相同。
18. **`blocks.hash`**：Rust 写的是 `""`，Python 写 content_hash 前 16 位。
    这是增量重建的信号，两壳全表比对时若漏掉这一列就会漏掉它。
19. **synthesis 懒编译层**（§4.3/§8.3）：`render_synthesis` /
    `mark_syntheses_stale` / `needs_recompile` / `recompile_synthesis` 全缺，
    Rust 之前**完全没有** synthesis 这一层。
20. **CLI 面/输出对齐**：`retract -b`、`studio -t`、`answer`(top=5)、
    `retract/answer/studio` 的输出文案、`render_faq` 模板、`compile` 输出格式。

### 可复现的工具

> 以下两点指向 `git show 0098f7d:tools/`（Python 侧已被删除）：与其联络起来
> 才能让本节的结论可复现。

```bash
cargo test --release --manifest-path rust-cli/Cargo.toml   # 20 suites, 146 tests
git show 0098f7d:tools/synth_diff.py > /tmp/synth_diff.py
git show 0098f7d:tools/gen_synthesis_fixture.py > /tmp/gen_synthesis_fixture.py
# synthesis 层的跨壳生命周期差分（Python 实跑 vs Rust 实跑）
python3 -u /tmp/synth_diff.py   # 期望 RESULT: IDENTICAL ✔
# 重新生成 synthesis 的 PY_* 常量（必须靠实跑 Python，不能靠读源码）
python3 /tmp/gen_synthesis_fixture.py
```

> **永远问"Rust 能不能做这件事"，而不是只问"做对了没有"。**
> 上面的差分法只能发现后者；14–20 全是得靠前者才挖出来的。

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
实测当前 Rust 二进制正是这一行（见 README 的 Known gaps 一节，那里用的是
`tests/channels_eval.rs` 的 fixture，因此数值为 refusal_acc=0.500 / citation_cov=0.434；
本节的 fixture 不同，两者都真实）。

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
# Rust —— 仓库里现在唯一的一套
cargo build --release --manifest-path rust-cli/Cargo.toml
cargo test --release --manifest-path rust-cli/Cargo.toml   # 20 suites, 146 tests
# Python —— 需要先把当时的源文件从历史里取回来（已被删除）
git show cddcb64:koiosbase/cli.py > /tmp/koios_cli.py      # 依此类推
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
