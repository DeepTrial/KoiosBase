[![CI](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/DeepTrial/KoiosBase)](https://github.com/DeepTrial/KoiosBase/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

English version | [中文版](README.md)

# KoiosBase

KoiosBase 是一个轻量级、Markdown 原生的 LLM 知识库系统。

主流 RAG 范式——「把文档切成 chunk、嵌入成向量、按相似度捞取 top-k」——丢弃了知识的
结构、关系与生命周期；后来叠加的各种补丁（混合检索、重排序、上下文化）只缓解症状。

KoiosBase 的立场不同：**把知识库当作一个持续构建的软件工程来对待**。

| 软件工程 | KoiosBase |
| --- | --- |
| 源代码仓库 | `raw/` —— 人维护的 Markdown Vault |
| 编译器 | 编译层 —— 摄入时把源文档综合为结构化知识页 |
| 构建产物 | `.index/` —— 向量、BM25、图索引，全部可重建 |
| 运行时 | 查询管线 —— 在构建产物上导航与推理 |
| 静态检查 / CI | lint 园丁 —— 周期性体检 |
| 版本控制 | Git + 版本链 |

## 与纯 RAG 的区别

| | 纯 RAG | KoiosBase |
| --- | --- | --- |
| 存储 | 扁平 chunk 池 | 三层知识模型（原料 / 编译 / 派生） |
| 检索 | 单次 top-k | 通道路由 + 充分性迭代 |
| 增长 | 只进不出 | 摄入编译 + 问答回流 + lint 自愈 |
| 纠错 | 重切重嵌 | 状态标记 + 沿引用链传播 |
| 信任 | 相似度分数 | 引用契约 + 页码级溯源 |

## 七条第一性原理

- **P1** Markdown 是唯一真相源。
- **P2** 存最细的，取任意粒度（粗粒度只是视图）。
- **P3** 检索是导航与推理，不是相似度竞赛。
- **P4** 知识在摄入时综合，不在查询时重推导。
- **P5** 知识有状态，错误可传播式修正。
- **P6** 一切断言可审计到原文。
- **P7** 判定者与生成者必须异源（反自证）。

## 安装

需要 Python 3.10+（SQLite 自带 FTS5）。

```bash
pip install -e ".[test]"
```

## 快速开始

```bash
koios init myvault          # 建库并写入 AGENTS.md 契约
# 编辑 myvault/raw/*.md
koios index myvault         # 构建派生索引（幂等）
koios search "营收" -p myvault
koios lint myvault          # L1 程序化健康检查
koios checkclaim "营收 32 亿元" "营收 32 亿元"
```

## 当前进度（v0.4）

已实现（对应 `docs/KoiosBase设计文档v1.3.md` 路线图 v0.4）：

- **知识状态机**（§8.2）：active / superseded / disputed / retracted / draft，
  以及页级 `stale` 标记
- **级联传播**（§8.3）：撤回一个 Block → 反向索引找出所有引用页 → 标 stale，
  精确手术而非重切重嵌
- **synthesis 懒更新**（§8.3）：源变更只标 stale，真正重编推迟到查询触达时
- **查询时状态处置**（§8.3）：stale 默认降权+异步修复；仅高风险/唯一证据源
  才升级为拒用或限域当场重编
- **园丁检查**（§9.3）：断链、无源断言、TTL 到期、stale 页、未决矛盾、孤儿页
- 编译层、质量门、答案回流、PPR 置信加权（v0.3）
- PDF 适配、页码溯源、sources/ 导读页（v0.2）、四通道检索、评测骨架（v0.1）
- 49 个 pytest 用例 + CI（ruff、3.10/3.11/3.12、PDF 专项）

```bash
koios retract -p myvault -b "r.md#财务/1" -r "数字错误"   # 级联传播（§8.3）
koios lint myvault                                        # 园丁体检（§9.3）
```

```bash
koios compile -p myvault                      # 编译 entities/（§5.3）
koios promote wiki/entities/acme.md --verified # 晋升（须验证/人工，§5.4）
koios answer -p myvault -q "..." -a "..."      # 答案回流（§6.6）
```

```bash
koios search "营收" -p myvault -c tree     # 指定通道 ①
koios eval -p myvault                      # 评测基线（needs-llm 题计入不计过）
koios eval -p myvault --strict             # 连已知语义缺口也算失败
```

诚实标注的已知限制：`koios eval` 输出 `needs_llm` 计数。这类问题关键词证据会
误判（例如语料提到「公司」≠ 能回答「分红政策」），区分「提到」与「回答」要等 v0.3
的跨家族 reranker/Grader；当前把它们计为缺口，不静默放过。

尚未实现（后续版本）：编译层（entities/concepts，含通道 ⓪ wiki 优先与 LLM 撰写的
导航式摘要）、PDF 适配器、VLM 抽检环；跨家族 L2 判定目前是确定性占位实现（仅实现接口）。
v0.1 的通道 ① 树导航已由首段派生摘要驱动（无需 LLM），Grader 为确定性判据。

## 目录结构

```
koiosbase/
  core/        Block / Section / Document 数据模型
  parsers/     格式适配器（当前 Markdown）
  ingest/      raw + wiki -> 派生索引
  index/       SQLite schema（树、块、全文、链接）
  retrieval/   BM25、RRF 融合、个性化 PageRank
  generation/  引用/拒答契约、跨家族判定
  lint/        L1 程序化园丁
  query/       检索 -> 组装 -> 生成 管线
  cli.py       koios 命令行
```

## 文档

- `docs/KoiosBase设计文档v1.3.md` —— 完整设计基线

## 许可证

MIT —— 见 [LICENSE](LICENSE)。
