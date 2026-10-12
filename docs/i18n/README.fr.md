[![CI](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/DeepTrial/KoiosBase)](https://github.com/DeepTrial/KoiosBase/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)

[中文](../../README.md) | [English](README.en.md) | [Français](README.fr.md) | [Español](README.es.md) | [日本語](README.ja.md) | [한국어](README.ko.md)

# KoiosBase

Une base de connaissances qui traite le Markdown comme source de vérité. Vous
écrivez du Markdown, KoiosBase l'indexe, et chaque réponse renvoie au bloc dont
elle provient.

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

- **Le Markdown est ce qui compte.** L'index peut être supprimé à tout moment et
  revient identique.
- **La connaissance périt.** Rétractez un bloc et chaque page qui le cite est
  marquée obsolète et cesse de répondre.

```mermaid
flowchart LR
    A["raw/<br/>(vous écrivez)"] --> B["koios index"]
    B --> C[".index/<br/>(artefact de build)"]
    C --> D["recherche / réponse"]
    D --> E["réponse + citation"]
    B --> F["wiki/<br/>(pages compilées)"]
    F --> D
    style A fill:#e8f4ea,stroke:#4a7c59
    style F fill:#fff6e5,stroke:#8a6d3b
```

## Installation

Un unique binaire Rust. Aucune dépendance à l'exécution.

```bash
curl -LO https://github.com/DeepTrial/KoiosBase/releases/latest/download/koios-linux-x86_64
chmod +x koios-linux-x86_64
# ou depuis les sources (le bindgen de MuPDF nécessite libclang)
git clone https://github.com/DeepTrial/KoiosBase && cd KoiosBase
cargo build --release --manifest-path rust-cli/Cargo.toml
```

```console
$ koios init demo && koios index demo && koios lint demo
initialized KoiosBase vault at demo
indexed 0 blocks from demo
---- lint: 0 finding(s)
```

### Brancher un modèle (optionnel)

Ça marche sans. Le générateur intégré renvoie les blocs retrouvés tels quels,
avec leurs citations. Il n'invente rien.

Pour en brancher un, `koios config --init -p myvault` écrit un `koios.toml`
commenté. Il suffit de le modifier :

```toml
[model]
kind = "openai"                          # voir les deux protocoles ci-dessous
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"
api_key_env = "OPENAI_API_KEY"           # le NOM de la variable d'environnement, jamais la clé
```

Une fois ce fichier présent, `koios query` utilise le modèle sans option
supplémentaire. `kind` prend exactement deux valeurs et décide du protocole :

| `kind` | requête POST vers | réponse lue depuis |
| --- | --- | --- |
| `openai` (par défaut) | `{base}/chat/completions` | `choices[0].message.content` |
| `anthropic` | `{base}/messages` | `content[0].text` |

`openai` couvre tout ce qui parle ce protocole : OpenAI, Ollama, vLLM,
llama.cpp, DeepSeek, Moonshot, LiteLLM — ainsi que les passerelles qui proxifient
Claude, qui exposent cette forme quel que soit le modèle derrière. Seul
l'endpoint propre à Anthropic exige `anthropic`. Gemini et Bedrock ne sont pas
implémentés ; utilisez `--llm-cmd` ci-dessous, ou une passerelle qui traduit.

Pour un essai ponctuel sans éditer de fichier, `--llm-cmd` prend la priorité sur
la configuration :

```console
$ koios query "revenue" -p myvault --llm-cmd 'printf "%s" "Revenue was 3.2 billion yuan."'
verdict: enough  hits: 4  escalated: false
Revenue was 3.2 billion yuan.
[contracts] citation=0/1 sentences cited
```

La réponse du modèle passe par la même barrière. Une citation manquante est
signalée, pas acceptée en silence, et un modèle en panne est une erreur plutôt
qu'une réponse fabriquée. L'exemple ci-dessus est précisément recadré pour ça :
il ne porte aucune citation.

## Utilisation

```console
$ koios init myvault                            # créer un vault
$ koios index myvault                           # après chaque édition de raw/
$ koios search "revenue" -p myvault             # rechercher
$ koios query  "revenue" -p myvault             # pipeline complet + verdict
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
$ koios compile -p myvault                      # compiler les pages structurées
compiled: entities=2 entity_pages=2 source_pages=1 affected_pages=5 stamp=2026-10-10
$ koios studio brief -p myvault -t revenue      # exporter
wrote myvault/wiki/synthesis/brief-revenue.md
$ koios mcp --install codex                     # enregistrer auprès d'un hôte MCP
```

La recherche choisit un canal par question et demande si les preuves suffisent
avant de répondre :

```mermaid
flowchart TD
    Q[question] --> C{"choisir le canal"}
    C -->|①| T["tree<br/>suivre les titres"]
    C -->|②| B25["BM25<br/>termes"]
    C -->|③| G["graph<br/>PPR"]
    C -->|④| F["corpus complet"]
    T --> Gr{"preuves suffisantes ?"}
    B25 --> Gr
    G --> Gr
    F --> Gr
    Gr -->|enough| An["réponse + [[citation]]"]
    Gr -->|"evidence_absent"| Esc["escalade vers ④"]
    Esc --> Gr
    Gr -->|still absent| R[refus]
    style An fill:#e8f4ea,stroke:#4a7c59
    style R fill:#fdecea,stroke:#a94442
```

**ACL** : `--groups finance-team` indique à quel groupe vous appartenez. Sans
l'option vous êtes anonyme et les documents restreints restent invisibles. C'est
voulu.

**Structure du vault** : `raw/` est à vous, `wiki/` est généré et doit être
commité, `.index/` est un artefact de build et ne doit pas l'être.

## Maintenance

| Tâche | Commande |
| --- | --- |
| Après une édition de `raw/` | `koios index myvault` |
| Contrôle de santé | `koios lint myvault` |
| Après une montée de version | `koios index myvault --full` |
| Un fait est faux | `koios retract -p myvault -b "block-id" --reason "..."` |
| Élever la confiance | `koios promote --human-confirmed "wiki/entities/acme.md"` |

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

La confiance ne monte que sur des preuves extérieures au générateur (§5.4 —
jamais en comptant les citations) : `draft → medium → high`.

## Documentation

- `docs/KoiosBase设计文档v1.3.md` — référence de conception
- `docs/python-rust-parity.md` — la piste d'audit du portage
- `docs/known-gaps.md` — lacunes connues et limites
- `docs/i18n.md` — convention de traduction
- `AGENTS.md` — le contrat écrit dans chaque vault

MIT — voir [LICENSE](../../LICENSE).
