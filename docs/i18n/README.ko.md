[![CI](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/DeepTrial/KoiosBase)](https://github.com/DeepTrial/KoiosBase/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)

[中文](../../README.md) · [English](README.en.md) · [Français](README.fr.md) · [Español](README.es.md) · [日本語](README.ja.md) · 한국어

# KoiosBase

Markdown을 신뢰의 원천으로 다루는 지식 베이스입니다. Markdown을 작성하면
KoiosBase가 색인하고, 모든 답변은 그 근거가 된 블록을 가리킵니다.

```console
$ koios search "revenue" -p myvault
[0] report.md#Financials/Revenue/1
    2024 Annual Report > Financials > Revenue
    ACME Corp revenue in 2024 was 3.2 billion yuan, up 12 percent.
[1] sources/report.md#2024 Annual Report/覆盖章节/1
    2024 Annual Report > 2024 Annual Report > 覆盖章节
    - Financials (`report.md#Financials`) - Revenue (`report.md#Financials/Revenue`) - Cash Flow (`report.md#Finan
[2] sources/report.md#2024 Annual Report/本文档能回答的问题/1
    2024 Annual Report > 2024 Annual Report > 本文档能回答的问题
    - 关于「Financials」，本文档有哪些说明？ (report.md#Financials) - 关于「Revenue」，本文档有哪些说明？ (report.md#Financials/Revenue) - 关于「
[3] sources/report.md#2024 Annual Report/导读摘要/1
    2024 Annual Report > 2024 Annual Report > 导读摘要
    ACME Corp revenue in 2024 was 3. 2 billion yuan, up 12 percent. Operating cash flow was positive for the year.
```

- **Markdown이 기준입니다.** 색인은 빌드 산출물이므로 언제든 삭제할 수 있고,
  다시 만들면 동일하게 복원됩니다.
- **지식에는 수명이 있습니다.** 블록을 retract하면 그 블록을 인용한 모든 페이지가
  stale로 표시되어 답변에 사용되지 않습니다.

```mermaid
flowchart LR
    A["raw/<br/>(작성한 내용)"] --> B["koios index"]
    B --> C[".index/<br/>(빌드 산출물)"]
    C --> D["검색 / 답변"]
    D --> E["답변 + 인용"]
    B --> F["wiki/<br/>(컴파일된 페이지)"]
    F --> D
    style A fill:#e8f4ea,stroke:#4a7c59
    style F fill:#fff6e5,stroke:#8a6d3b
```

## 설치

단일 Rust 바이너리입니다. 런타임 의존성이 없습니다.

```bash
curl -LO https://github.com/DeepTrial/KoiosBase/releases/latest/download/koios-linux-x86_64
chmod +x koios-linux-x86_64
# 또는 소스에서 빌드 (MuPDF bindgen에는 libclang이 필요합니다)
git clone https://github.com/DeepTrial/KoiosBase && cd KoiosBase
cargo build --release --manifest-path rust-cli/Cargo.toml
```

```console
$ koios init demo && koios index demo && koios lint demo
initialized KoiosBase vault at demo
indexed 0 blocks from demo
---- lint: 0 finding(s)
```

### 모델 연결 (선택)

모델 없이도 동작합니다. 이 경우 내장 생성기는 검색된 블록을 인용과 함께 반환할
뿐이며, 원문에 없는 내용을 덧붙이지 않습니다.

모델을 연결하려면 `koios config --init -p myvault`를 실행합니다. 주석이 포함된
`koios.toml`이 생성되므로 필요한 항목만 수정하면 됩니다:

```toml
[model]
kind = "openai"                          # 프로토콜 (아래 표 참고)
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"
api_key_env = "OPENAI_API_KEY"           # 환경 변수의 '이름'. 키 자체가 아닙니다
```

이 파일이 있으면 `koios query`는 추가 옵션 없이 모델을 사용합니다. `kind`는
두 가지 값만 가지며 요청 프로토콜을 결정합니다:

| `kind` | 엔드포인트 | 답변을 읽는 위치 |
| --- | --- | --- |
| `openai` (기본값) | `{base}/chat/completions` | `choices[0].message.content` |
| `anthropic` | `{base}/messages` | `content[0].text` |

`openai`는 이 프로토콜을 사용하는 모든 서비스에 적용됩니다: OpenAI, Ollama,
vLLM, llama.cpp, DeepSeek, Moonshot, LiteLLM, 그리고 Claude를 프록시하는
게이트웨이(이들은 뒤의 모델과 무관하게 동일한 형식을 외부로 제공합니다).
`anthropic`은 Anthropic 자체 엔드포인트에서만 필요합니다. Gemini와 Bedrock은
지원하지 않습니다. 아래 `--llm-cmd`를 쓰거나 게이트웨이가 변환하도록 하세요.

설정 파일을 수정하지 않고 한 번만 시험하려면 `--llm-cmd`가 `koios.toml`보다
우선합니다:

```console
$ koios query "revenue" -p myvault --llm-cmd 'printf "%s" "Revenue was 3.2 billion yuan."'
verdict: enough  hits: 4  escalated: false
Revenue was 3.2 billion yuan.
[contracts] citation=0/1 sentences cited
```

인용 계약은 모델의 출력에도 적용됩니다. 인용이 누락되면 넘어가지 않고 기록·보고되며,
호출이 실패하면 생성된 텍스트로 대체하지 않고 오류를 반환합니다. 위 예시는 인용을
전혀 포함하지 않았기 때문에 지적된 것입니다.

## 사용법

```console
$ koios init myvault                            # vault 생성
$ koios index myvault                           # raw/를 편집한 후마다
$ koios search "revenue" -p myvault             # 검색
$ koios query  "revenue" -p myvault             # 전체 파이프라인 + verdict
verdict: enough  hits: 4  escalated: false
[0] report.md#Financials/Revenue/1
    ACME Corp revenue in 2024 was 3.2 billion yuan, up 12 percent.
[1] sources/report.md#2024 Annual Report/覆盖章节/1
    - Financials (`report.md#Financials`)
- Revenue (`report.md#Financials/Revenue`)
- Cash Flow (`report.md#Finan
[2] sources/report.md#2024 Annual Report/本文档能回答的问题/1
    - 关于「Financials」，本文档有哪些说明？ (report.md#Financials)
- 关于「Revenue」，本文档有哪些说明？ (report.md#Financials/Revenue)
- 关于「
[3] sources/report.md#2024 Annual Report/导读摘要/1
    ACME Corp revenue in 2024 was 3. 2 billion yuan, up 12 percent. Operating cash flow was positive for the year.
$ koios compile -p myvault                      # 구조화된 페이지 컴파일
compiled: entities=2 entity_pages=2 source_pages=1 affected_pages=5 refreshed=0 stamp=2026-10-12
$ koios studio brief -p myvault -t revenue      # 내보내기
wrote myvault/wiki/synthesis/brief-revenue.md
$ koios mcp --install codex                     # MCP 호스트에 등록
```

검색은 질문마다 채널을 선택하고, 답변하기 전에 증거가 충분한지 판단합니다:

```mermaid
flowchart TD
    Q[질문] --> C{"채널 선택"}
    C -->|①| T["tree<br/>제목 추적"]
    C -->|②| B25["BM25<br/>용어 일치"]
    C -->|③| G["graph<br/>PPR"]
    C -->|④| F["전체 코퍼스"]
    T --> Gr{"증거 충분?"}
    B25 --> Gr
    G --> Gr
    F --> Gr
    Gr -->|enough| An["답변 + [[인용]]"]
    Gr -->|"evidence_absent"| Esc["④로 에스컬레이션"]
    Esc --> Gr
    Gr -->|still absent| R[답변 거부]
    style An fill:#e8f4ea,stroke:#4a7c59
    style R fill:#fdecea,stroke:#a94442
```

**ACL**: `--groups finance-team`은 호출자의 그룹을 선언합니다. 지정하지 않으면
호출자는 익명이 되어 ACL로 제한된 문서를 볼 수 없습니다. 이것이 의도된 동작입니다.

**vault 구조**: `raw/`는 작성한 문서, `wiki/`는 생성된 페이지(커밋 권장),
`.index/`는 빌드 산출물(커밋하지 마세요).

## 유지보수

| 작업 | 명령 |
| --- | --- |
| `raw/` 편집 후 | `koios index myvault` |
| 상태 점검 | `koios lint myvault` |
| 버전 업데이트 후 | `koios index myvault --full` |
| 사실이 틀렸을 때 | `koios retract -p myvault -b "블록 ID" --reason "..."` |
| 신뢰도 상승 | `koios promote --human-confirmed "wiki/entities/acme.md"` |

```console
$ koios retract -p myvault -b "report.md#Financials/Revenue/1" --reason "wrong figure"
retracted report.md#Financials/Revenue/1; affected pages: 3 ["entities/acme-corp.md", "entities/acme.md", "synthesis/brief-revenue.md"]
```

```mermaid
stateDiagram-v2
    [*] --> draft: compiled
    draft --> medium: --verified
    medium --> high: --human
    active --> disputed: challenged
    active --> superseded: replaced
    active --> retracted: wrong
    superseded --> [*]: filtered from retrieval
    retracted --> [*]: filtered from retrieval
```

신뢰도는 생성기 외부의 증거로만 올라갑니다(§5.4 — 인용 개수는 반영하지 않습니다):
`draft → medium → high`.

## 문서

- `docs/KoiosBase设计文档v1.3.md` — 설계 기준
- `docs/python-rust-parity.md` — 포팅 감사 기록
- `docs/known-gaps.md` — 알려진 제한
- `docs/i18n.md` — 번역 규칙
- `AGENTS.md` — 모든 vault에 기록되는 계약

MIT — [LICENSE](../../LICENSE)를 참조하세요.
