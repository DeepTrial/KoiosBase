[![CI](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/DeepTrial/KoiosBase)](https://github.com/DeepTrial/KoiosBase/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[English](README.md) | [中文](README.zh.md) | [日本語](README.ja.md)

# KoiosBase

KoiosBase 是一个轻量、Markdown 原生的 LLM 知识库。

主流 RAG 范式——*把文档切成块、嵌入、按相似度取 top-k*——丢掉了结构、关系与
生命周期。此后叠加的每一项修补（混合检索、重排、块上下文化）都只治症状。

KoiosBase 采取另一种立场：**把知识库当作被持续构建的软件工程**。

| 软件工程 | KoiosBase |
| --- | --- |
| 源码仓库 | `raw/` — 人工维护的 Markdown（vault） |
| 编译器 | 编译层 — 在 ingest 时把源文档合成为结构化页面 |
| 构建产物 | `.index/` — BM25、图；始终可重建 |
| 运行时 | 查询流水线 — 在产物上导航与推理 |
| Lint / CI | gardener — 周期性健康检查 |
| 版本控制 | Git + 版本链 |

## 与普通 RAG 的差异

| | 普通 RAG | KoiosBase |
| --- | --- | --- |
| 存储 | 扁平块池 | 三层模型（raw / 编译 / 派生） |
| 检索 | 一次性 top-k | 通道路由 + 充分性迭代 |
| 增长 | 只增不改 | ingest 编译 + 答案回流 + lint 自愈 |
| 纠错 | 重新切块、重新嵌入 | 状态标记 + 沿引用链传播 |
| 可信度 | 相似度分数 | 引用契约 + 页级溯源 |

## 第一性原理

所有机制都由七条原则推导而来，不允许拼凑组件：

- **P1** Markdown 是唯一真相源。
- **P2** 以最细粒度存储；以任意粒度检索（视图）。
- **P3** 检索是导航与推理，不是相似度竞赛。
- **P4** 知识在 ingest 时合成，不在每次查询时重算。
- **P5** 知识有状态；错误会传播，且可修复。
- **P6** 每条断言都可回溯到源头。
- **P7** 判定者必须与生成者不同源（反自证）。

## 安装

需要 Python 3.10+（SQLite 带 FTS5 —— CPython 已内置）。

```bash
pip install -e ".[test]"
```

每个 [release](https://github.com/DeepTrial/KoiosBase/releases) 附带预编译二进制
（无需 Python）：`koios-linux-x86_64`（musl 静态）与 `koios-windows-x86_64.exe`。

## 快速开始

```bash
koios init myvault          # 搭建 vault + AGENTS.md 契约
# 编辑 myvault/raw/*.md
koios index myvault         # 构建派生索引（幂等）
koios search "营收" -p myvault
koios lint myvault          # L1 程序化健康检查
koios checkclaim "营收 32 亿元" "营收 32 亿元"
```

## 两个 shell，同一个 vault

KoiosBase 同时提供 Python 实现与 Rust 实现。两者读写**相同**的 vault 格式——
`raw/` + `wiki/` 是真相，`.index/` 是派生产物，因此任一侧建索引、任一侧查询都
可以。

Python CLI 是参考实现，覆盖全部命令。Rust 二进制覆盖同样的命令面，并在共享
fixture 上与 Python 输出逐项比对（block id、breadcrumb、生成的页面、MCP 响应
均为字节级比对）。

```bash
koios search "营收" -p myvault -c tree     # 强制指定通道（§6.1）
koios search "营收" -p myvault --groups finance-team   # ACL 主体（§9.2）
koios eval -p myvault                      # 评测基线
koios eval -p myvault --strict             # 已知语义缺口也算失败
```

## 当前范围（v0.8.1）

- **ACL / 多租户**（§9.2）：在 frontmatter 中声明，**在检索时**强制——受限文本
  永不进入模型（生成后再遮蔽会以转述形式泄漏）。派生 `sources/` 页继承其源
  raw 文档的授权：生成的页面本身不声明 `acl`，若按字面读取会把受限内容判为
  公开。
- **知识状态机**（§8.2/§8.3）：块带 `active | superseded | disputed |
  retracted | draft`；撤回沿引用链传播。被撤回的块在所有读路径上硬过滤。引用
  了「源文档已变动」页面的回答会标注（待更新）。
- **MCP server**（§11）：`koios mcp` 通过 stdio 讲 JSON-RPC，暴露
  `koios_search`、`koios_ask`、`koios_write_answer`。直接按协议实现——不依赖
  SDK，可离线运行。
- **Studio 导出**（§13）：`koios studio brief|mindmap` ——不新增断言且带引用的
  投影（P6）。两者均过 ACL：导出会落盘，因此那里的泄漏比生成侧泄漏更持久。
- **编译层**（§5.3/§5.4）：实体页与源页；晋升必须经跨族验证或人工确认，绝不
  按引用计数。重编译保留页面已晋升的 confidence 与人工区段。
- **PDF 适配器**（§5.1）：CPU 文本档 + 页级溯源，引用可指向确切页码。VLM
  档是 hook，不是依赖。
- **四通道**（§6.1）+ Grader 驱动的 Self-Route 升级（§6.2）。
- 65 条 pytest 用例 + CI（ruff、Python 3.10/3.11/3.12、PDF job）。

### 已知缺口 —— 如实登记，不隐藏

以下均为已记录的边界，不是疏漏。依赖本系统前请先阅读。

- **`needs_llm` 用例。** `koios eval` 会报 `needs_llm` 计数：那些关键词证据
  命中、但实际没有答案的问题（提到「公司」≠ 回答「分红政策」问题）。区分
  「提及」与「回答」需要 v0.3 的跨族 reranker/Grader，因此它们被计为缺口而非
  静默通过。
- **L2 judge 是确定性占位实现。** 它实现了接口，只判定可程序化检验的关系
  （数字），其余诚实地返回 `unknown`。
- **编译写集仍是 O(全库)。** 实体页整体重编译；正确性（保留 confidence 与人工
  区段）已解决，但受影响集算出来后并未用于收窄写入。作为技术债登记，见
  `docs/KoiosBase设计文档v1.3.md`。
- **stale 的异步重编译未实现。** 陈旧页会被标注并降权，但清除它的重编译不会
  自动触发（§5.3 禁止为此阻塞可用性）。
- **Windows 二进制仅做了结构验证。** 它能构建且是合法的 PE32+ 可执行文件，
  但当时没有 Windows runner 或 Wine 可实际执行它。

## 目录结构

```
koiosbase/
  core/        Block / Section / Document 数据模型
  parsers/     格式适配器（Markdown、PDF）
  ingest/      raw + wiki -> 派生索引
  index/       SQLite schema（tree、blocks、FTS、links）
  retrieval/   BM25 检索、RRF 融合、个性化 PageRank
  generation/  引用与拒答契约、跨族 judge
  compile/     实体页与源页、质量闸门
  state/       知识状态机与级联
  lint/        L1 程序化 gardener
  query/       retrieve -> assemble -> generate 流水线
  security/    ACL / 多租户
  mcp/         stdio JSON-RPC server
  studio/      brief / mindmap 导出
  cli.py       koios 命令行接口
rust-cli/      Rust shell（同一 vault 格式）
```

## 文档

- `docs/KoiosBase设计文档v1.3.md` — 完整设计基线（中文）
- `AGENTS.md` — 写入每个 vault 的维护契约（§4.4）

## 许可

MIT — 见 [LICENSE](LICENSE)。
