[![CI](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/DeepTrial/KoiosBase)](https://github.com/DeepTrial/KoiosBase/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[English](README.md) | [中文](i18n/README.zh.md) | [日本語](i18n/README.ja.md)

# KoiosBase

KoiosBase は、軽量で Markdown ネイティブな LLM ナレッジベースです。

主流の RAG パターン——*文書をチャンクに分割し、埋め込み、類似度で top-k を
取得する*——は、構造・関係・ライフサイクルを捨てています。その上に重ねられる
どんな修正（ハイブリッド検索、リランキング、チャンクの文脈化）も対症療法です。

KoiosBase は別の立場をとります。**ナレッジベースを、継続的にビルドされる
ソフトウェアエンジニアリングとして扱う**のです。

| ソフトウェアエンジニアリング | KoiosBase |
| --- | --- |
| ソースリポジトリ | `raw/` — 人が保守する Markdown（vault） |
| コンパイラ | コンパイル層 — ingest 時にソースを構造化ページへ合成 |
| ビルド成果物 | `.index/` — BM25、グラフ。常に再構築可能 |
| ランタイム | クエリパイプライン — 成果物上を导航し推論する |
| Lint / CI | gardener — 周期的なヘルスチェック |
| バージョン管理 | Git + バージョンチェーン |

## 通常の RAG との違い

| | 通常の RAG | KoiosBase |
| --- | --- | --- |
| 格納 | フラットなチャンクプール | 三層モデル（raw / コンパイル / 派生） |
| 検索 | ワンショット top-k | チャネルルーティング + 十分性の反復 |
| 成長 | 追記のみ | ingest コンパイル + 回答フィードバック + lint 自己修復 |
| 訂正 | 再チャンク・再埋め込み | 状態フラグ + 引用リンク沿いの伝播 |
| 信頼 | 類似度スコア | 引用契約 + ページレベルの来歴 |

## 第一原理

すべてのメカニズムは七つの原理から導かれ、寄せ集めのコンポーネントは許されません：

- **P1** Markdown が唯一の情報源である。
- **P2** 最小粒度で保存し、任意の粒度で検索する（ビュー）。
- **P3** 検索は导航と推論であり、類似度コンテストではない。
- **P4** 知識は ingest 時に合成され、クエリごとに再計算されない。
- **P5** 知識は状態を持ち、誤りは伝播し修復可能である。
- **P6** すべての主張は情報源まで監査可能である。
- **P7** 判定者は生成者と起源が異ならなければならない（自己検証の禁止）。

## インストール

Python 3.10+ が必要です（SQLite は FTS5 付き —— CPython に同梱）。

```bash
pip install -e ".[test]"
```

各 [リリース](https://github.com/DeepTrial/KoiosBase/releases) にはビルド済み
バイナリ（Python 不要）が添付されています：`koios-linux-x86_64`（musl 静的）
および `koios-windows-x86_64.exe`。

## クイックスタート

```bash
koios init myvault          # vault と AGENTS.md 契約を作成
# myvault/raw/*.md を編集
koios index myvault         # 派生インデックスを構築（冪等）
koios search "营收" -p myvault
koios lint myvault          # L1 プログラム的ヘルスチェック
koios checkclaim "营收 32 亿元" "营收 32 亿元"
```

## 二つのシェル、一つの vault

KoiosBase は Python 実装と Rust 実装を同梱しています。どちらも**同じ** vault
形式を読み書きします —— `raw/` + `wiki/` が真実で、`.index/` は派生成果物なので、
どちらでインデックスしどちらで検索しても構いません。

Python CLI がリファレンス実装で、全コマンドを網羅します。Rust バイナリも同じ
コマンド面を網羅し、共有フィクスチャ上で Python の出力と照合されています
（ブロック ID、breadcrumb、生成ページ、MCP 応答をバイト単位で比較）。

```bash
koios search "营收" -p myvault -c tree     # チャネルを強制（§6.1）
koios search "营收" -p myvault --groups finance-team   # ACL プリンシパル（§9.2）
koios eval -p myvault                      # 評価ベースライン
koios eval -p myvault --strict             # 既知の意味的ギャップも失敗にする
```

## 現在のスコープ（v0.8.1）

- **ACL / マルチテナンシー**（§9.2）：frontmatter で宣言し、**検索時に**強制
  —— 制限されたテキストは決してモデルに届きません（生成後の隠蔽は言い換えで
  漏れるため）。派生 `sources/` ページは、コンパイル元の raw 文書の権限を継承
  します。生成ページ自身は `acl` を宣言しないため、文字通り読むと制限コンテンツ
  を公開と判定してしまいます。
- **知識状態機械**（§8.2/§8.3）：ブロックは `active | superseded | disputed |
  retracted | draft` を持ち、撤回は引用リンク沿いに伝播します。撤回された
  ブロックはすべての読み取り経路でハードフィルタされます。情報源が更新された
  ページを引用する回答には（待更新）が付きます。
- **MCP サーバー**（§11）：`koios mcp` は stdio 経由で JSON-RPC を話し、
  `koios_search`、`koios_ask`、`koios_write_answer` を公開します。プロトコルに
  直接実装しているため SDK 依存がなく、オフラインで動作します。
- **Studio エクスポート**（§13）：`koios studio brief|mindmap` —— 新しい主張を
  追加せず引用を伴う投影（P6）。どちらも ACL でフィルタされます。エクスポートは
  ディスクに書き出されるため、そこでの漏洩はリクエストより長く残ります。
- **コンパイル層**（§5.3/§5.4）：エンティティページとソースページ。昇格には
  クロスファミリ検証または人による確認が必要で、決して引用数ではありません。
  再コンパイルはページの昇格済み confidence と人手の記述を保持します。
- **PDF アダプタ**（§5.1）：CPU テキスト層とページレベルの来歴により、引用は
  正確なページを指します。VLM 層は依存ではなくフックです。
- **四つのチャネル**（§6.1）+ Grader 駆動の Self-Route エスカレーション（§6.2）。
- 65 件の pytest ケース + CI（ruff、Python 3.10/3.11/3.12、PDF ジョブ）。

### 既知のギャップ —— 隠さず記録する

以下は記録済みの限界であり、見落としではありません。本システムに依存する前に
お読みください。

- **`needs_llm` ケース。** `koios eval` は `needs_llm` 数を報告します。これは
  キーワード証拠がヒットするものの実際の答えがない質問です（「会社」に言及
  ≠ 配当方針の質問に回答）。「言及」と「回答」の区別には v0.3 のクロスファミリ
  reranker/Grader が必要なため、これらは黙って合格させるのではなくギャップと
  して計上されます。
- **L2 judge は決定的なプレースホルダです。** インターフェースを実装し、
  プログラム的に検証可能な関係（数値）のみを判定し、それ以外は正直に
  `unknown` を返します。
- **コンパイルの書き込み集合は O(コーパス全体) のままです。** エンティティ
  ページは全体が再コンパイルされます。正当性（confidence と人手セクションの
  保持）は解決済みですが、影響集合は計算されるものの書き込みの絞り込みには
  使われていません。技術的負債として記録済みです。
  `docs/KoiosBase设计文档v1.3.md` を参照してください。
- **stale 時の非同期再コンパイルは未実装です。** 古いページは注記され降格され
  ますが、それを解消する再コンパイルは自動的に発火しません（§5.3 が可用性の
  ブロックを禁じているため）。
- **Windows バイナリは構造的にのみ検証済みです。** ビルドされ有効な PE32+
  実行ファイルですが、実行に Windows ランナーも Wine も利用できませんでした。

## 構成

```
koiosbase/
  core/        Block / Section / Document データモデル
  parsers/     フォーマットアダプタ（Markdown、PDF）
  ingest/      raw + wiki -> 派生インデックス
  index/       SQLite スキーマ（tree、blocks、FTS、links）
  retrieval/   BM25 検索、RRF 融合、パーソナライズド PageRank
  generation/  引用・拒答契約、クロスファミリ judge
  compile/     エンティティ/ソースページ、品質ゲート
  state/       知識状態機械とカスケード
  lint/        L1 プログラム的 gardener
  query/       retrieve -> assemble -> generate パイプライン
  security/    ACL / マルチテナンシー
  mcp/         stdio JSON-RPC サーバー
  studio/      brief / mindmap エクスポート
  cli.py       koios コマンドラインインターフェース
rust-cli/      Rust シェル（同じ vault 形式）
```

## ドキュメント

- `docs/KoiosBase设计文档v1.3.md` — 完全な設計ベースライン（中国語）
- `docs/i18n.md` — 翻訳ドキュメントの構成規約（翻訳は `i18n/` に配置）
- `AGENTS.md` — 各 vault に書き込まれる保守契約（§4.4）

## ライセンス

MIT — [LICENSE](LICENSE) を参照してください。
