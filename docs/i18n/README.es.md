[![CI](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/DeepTrial/KoiosBase)](https://github.com/DeepTrial/KoiosBase/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](../../LICENSE)

[中文](../../README.md) · [English](README.en.md) · [Français](README.fr.md) · Español · [日本語](README.ja.md) · [한국어](README.ko.md)

# KoiosBase

Base de conocimiento que trata Markdown como fuente de verdad. Escribes
Markdown, KoiosBase lo indexa y cada respuesta apunta al bloque del que proviene.

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

- **Markdown es lo que cuenta.** El índice puede borrarse en cualquier momento y
  se reconstruye de forma idéntica.
- **El conocimiento caduca.** Retraes un bloque y todas las páginas que lo citan
  quedan marcadas como obsoletas y dejan de responder.

```mermaid
flowchart LR
    A["raw/<br/>(tú escribes)"] --> B["koios index"]
    B --> C[".index/<br/>(artefacto de build)"]
    C --> D["búsqueda / respuesta"]
    D --> E["respuesta + cita"]
    B --> F["wiki/<br/>(páginas compiladas)"]
    F --> D
    style A fill:#e8f4ea,stroke:#4a7c59
    style F fill:#fff6e5,stroke:#8a6d3b
```

## Instalación

Un único binario de Rust, sin dependencias en tiempo de ejecución.

```bash
curl -LO https://github.com/DeepTrial/KoiosBase/releases/latest/download/koios-linux-x86_64
chmod +x koios-linux-x86_64
# o compilar desde el código fuente (el bindgen de MuPDF necesita libclang)
git clone https://github.com/DeepTrial/KoiosBase && cd KoiosBase
cargo build --release --manifest-path rust-cli/Cargo.toml
```

```console
$ koios init demo && koios index demo && koios lint demo
initialized KoiosBase vault at demo
indexed 0 blocks from demo
---- lint: 0 finding(s)
```

### Conectar un modelo (opcional)

KoiosBase funciona sin modelo. En ese caso el generador integrado devuelve los
bloques recuperados con sus citas, sin añadir nada más allá del texto original.

Para conectar uno, ejecuta `koios config --init -p myvault`. El comando escribe
un `koios.toml` comentado; basta con modificar los campos necesarios:

```toml
[model]
kind = "openai"                          # protocolo; ver la tabla siguiente
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"
api_key_env = "OPENAI_API_KEY"           # el NOMBRE de la variable de entorno, nunca la clave
```

Una vez que ese archivo existe, `koios query` usa el modelo sin opciones
adicionales. `kind` admite exactamente dos valores y selecciona el protocolo de
la petición:

| `kind` | endpoint | respuesta leída desde |
| --- | --- | --- |
| `openai` (por defecto) | `{base}/chat/completions` | `choices[0].message.content` |
| `anthropic` | `{base}/messages` | `content[0].text` |

`openai` se aplica a cualquier servicio que hable ese protocolo: OpenAI, Ollama,
vLLM, llama.cpp, DeepSeek, Moonshot, LiteLLM y las pasarelas que proxifican
Claude, las cuales exponen esta misma forma con independencia del modelo que
tengan detrás. `anthropic` solo es necesario para el endpoint propio de
Anthropic. Gemini y Bedrock no están cubiertos; usa `--llm-cmd` más abajo o una
pasarela que traduzca por ti.

Para una prueba puntual sin modificar el archivo de configuración, `--llm-cmd`
tiene prioridad sobre `koios.toml`:

```console
$ koios query "revenue" -p myvault --llm-cmd 'printf "%s" "Revenue was 3.2 billion yuan."'
verdict: enough  hits: 4  escalated: false
Revenue was 3.2 billion yuan.
[contracts] citation=0/1 sentences cited
```

Los contratos de cita también se aplican a la salida del modelo. Una cita
ausente se registra y se reporta en lugar de pasarse por alto, y una llamada
fallida devuelve un error en vez de sustituir texto generado. El ejemplo de
arriba está marcado precisamente porque no lleva ninguna cita.

## Uso

```console
$ koios init myvault                            # crear un vault
$ koios index myvault                           # tras cada edición de raw/
$ koios search "revenue" -p myvault             # buscar
$ koios query  "revenue" -p myvault             # pipeline completo + verdict
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
$ koios compile -p myvault                      # compilar páginas estructuradas
compiled: entities=2 entity_pages=2 source_pages=1 affected_pages=5 refreshed=0 stamp=2026-10-12
$ koios studio brief -p myvault -t revenue      # exportar
wrote myvault/wiki/synthesis/brief-revenue.md
$ koios mcp --install codex                     # registrar en un host MCP
```

La recuperación elige un canal por pregunta y determina si la evidencia es
suficiente antes de responder:

```mermaid
flowchart TD
    Q[pregunta] --> C{"elegir canal"}
    C -->|①| T["tree<br/>seguir títulos"]
    C -->|②| B25["BM25<br/>términos"]
    C -->|③| G["graph<br/>PPR"]
    C -->|④| F["corpus completo"]
    T --> Gr{"¿evidencia suficiente?"}
    B25 --> Gr
    G --> Gr
    F --> Gr
    Gr -->|enough| An["respuesta + [[cita]]"]
    Gr -->|"evidence_absent"| Esc["escalar a ④"]
    Esc --> Gr
    Gr -->|still absent| R[rechazar]
    style An fill:#e8f4ea,stroke:#4a7c59
    style R fill:#fdecea,stroke:#a94442
```

**ACL**: `--groups finance-team` declara el grupo del llamante. Si se omite, el
llamante es anónimo y los documentos restringidos por ACL permanecen invisibles.
Es el comportamiento esperado.

**Estructura del vault**: `raw/` contiene lo que escribes, `wiki/` las páginas
generadas (haz commit), `.index/` es un artefacto de build (no lo comitees).

## Mantenimiento

| Tarea | Comando |
| --- | --- |
| Tras editar `raw/` | `koios index myvault` |
| Comprobación de estado | `koios lint myvault` |
| Tras actualizar de versión | `koios index myvault --full` |
| Un hecho es incorrecto | `koios retract -p myvault -b "id-del-bloque" --reason "..."` |
| Elevar la confianza | `koios promote --human-confirmed "wiki/entities/acme.md"` |

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

La confianza solo sube con evidencia externa al generador (§5.4 — nunca contando
citas): `draft → medium → high`.

## Documentación

- `docs/KoiosBase设计文档v1.3.md` — base de diseño
- `docs/python-rust-parity.md` — traza de auditoría del port
- `docs/known-gaps.md` — limitaciones conocidas
- `docs/i18n.md` — convención de traducción
- `AGENTS.md` — el contrato escrito en cada vault

MIT — véase [LICENSE](../../LICENSE).
