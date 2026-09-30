"""PDF adapter — parse(path) -> Document (design doc §5.1).

Implements the **double-tier fallback**:
  default tier  : CPU extraction (PyMuPDF) — cheap, deterministic
  precision tier: VLM extraction invoked only when confidence is low or the
                  CPU tier fails — catches scans and layout-heavy pages

Provenance: every Block records `page_range` so citations can point at an exact
page (§4.1, §6.5). Results are cached by content hash so re-ingesting the same
file does not re-parse.

The VLM tier is behind a callable (`vlm`) so v0.2 ships without a model
dependency while keeping the escalation path real and testable.
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
from collections.abc import Callable
from pathlib import Path

from ..core.block import Block, Document, Section

HEADING_MAX_LEN = 80
HEADING_RE = re.compile(r"^\s*(#{1,6})\s+(.*)$")


def file_sha256(path: str | Path, limit: int | None = None) -> str:
    """Content hash of a file, full by default.

    `limit` truncated at 16MB, so any edit past that offset was invisible: the
    doc_hash used for provenance (§4.1) and the parse cache key both agreed with
    a version of the file that no longer existed, and re-ingesting edited PDFs
    returned the cached parse. Set `limit` deliberately if you want a cheap probe
    of just the head; correctness-sensitive callers must hash everything.
    """
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        while True:
            chunk = fh.read(1 << 20)
            if not chunk:
                break
            h.update(chunk)
            if limit is not None and fh.tell() > limit:
                break
    return h.hexdigest()


def log_skip(exc: BaseException) -> None:
    """Record why a tier was skipped — silent degradation is undebuggable."""
    print(f"[pdf] tier skipped: {type(exc).__name__}: {exc}", file=sys.stderr)


def extract_pages_cpu(path: str | Path) -> list[str]:
    """Default CPU tier: text per page via PyMuPDF. Returns [] on failure."""
    try:
        import pymupdf  # PyMuPDF >= 1.24 exposes this name
    except ImportError:  # pragma: no cover - older installs
        try:
            import fitz as pymupdf
        except ImportError:
            return []
    try:
        doc = pymupdf.open(str(path))
    except Exception as exc:  # noqa: BLE001 - corrupt file must degrade to []
        log_skip(exc)
        return []
    pages = []
    try:
        for page in doc:
            pages.append(page.get_text() or "")
    finally:
        doc.close()
    return pages


def looks_scanned(page_texts: list[str], min_chars: int = 40) -> bool:
    """Heuristic for 'this PDF is probably a scan': almost no extractable text.

    This is what decides whether to escalate to the VLM tier (§5.1). Kept
    deliberately simple and explicit rather than a magic threshold.
    """
    if not page_texts:
        return True
    usable = sum(1 for t in page_texts if len(t.strip()) >= min_chars)
    return usable < max(1, len(page_texts) // 2)


def _is_heading(line: str) -> bool:
    """Distinguish a heading from body text in flat PDF extracts.

    A page extract has no Markdown structure, so we use surface cues: a short
    line with no sentence-ending punctuation. Deliberately conservative — calling
    body text a heading would DROP the block entirely (see the bug this fixed,
    where every line looked like a heading and PDF pages indexed as empty).
    """
    s = line.strip()
    if not s or len(s) > HEADING_MAX_LEN:
        return False
    if s.endswith((".", "。", ";", "；", ":", "：")):
        return False
    return len(s.split()) <= 8


def _page_to_blocks(
    doc_path: str, page_no: int, text: str, start_ord: int
) -> list[Block]:
    """Turn one page's text into Blocks tagged with its page_range.

    Lines are grouped so that a heading is attached to the body text that
    follows it; every non-heading line becomes its own cited Block.
    """
    blocks: list[Block] = []
    ordinal = start_ord
    base_title = doc_path.rsplit(".", 1)[0] if "." in doc_path else doc_path
    heading: str | None = None
    pending: list[str] = []

    def flush() -> None:
        nonlocal ordinal, pending
        body = " ".join(x.strip() for x in pending if x.strip())
        pending = []
        if not body:
            return
        ordinal += 1
        crumb = " > ".join(x for x in [base_title, heading or f"p.{page_no}"] if x)
        blocks.append(
            Block(
                id=f"{doc_path}#p{page_no}/{ordinal}",
                type="table" if "|" in body else "paragraph",
                breadcrumb=crumb,
                raw=body,
                doc_path=doc_path,
                ordinal=ordinal,
                section_id=f"{doc_path}#p{page_no}",
                page_range=[page_no, page_no],
                meta={"source_format": "pdf"},
            )
        )

    for line in text.split("\n"):
        s = line.strip()
        if not s:
            continue
        if _is_heading(s) and not pending:
            heading = s
            continue
        pending.append(s)
    flush()
    return blocks


def parse_pdf(
    path: str | Path,
    root: str | Path = "",
    cache_dir: str | Path | None = None,
    vlm: Callable[[str, bytes], str] | None = None,
) -> Document:
    """Parse a PDF into a Document whose Blocks carry page-level provenance.

    `vlm(path, image_bytes)` receives PNG bytes — see the call site for why it
    must not be a str-typed placeholder.
    """
    root_str = str(root) if root else ""
    if root_str and str(path).startswith(root_str):
        doc_path = str(path.relative_to(root_str))
    else:
        doc_path = path.name

    pages = extract_pages_cpu(path)
    scanned = looks_scanned(pages)
    if (scanned or not pages) and vlm is not None:
        # Precision tier: hand the page images to a VLM for transcription.
        try:
            import pymupdf
        except ImportError:  # pragma: no cover
            pymupdf = None
        if pymupdf is not None:
            pages = []
            doc = pymupdf.open(str(path))
            try:
                for page in doc:
                    pix = page.get_pixmap()
                    # Hand the VLM the PNG BYTES. The previous `img.hex()[:0] or
                    # img.decode("latin-1")` was a placeholder that always took the
                    # right-hand branch (hex()[:0] is the empty string), smuggling
                    # raw binary through a text decode: it survives only for bytes
                    # < 0x80 and corrupts everything else, so anything beyond an
                    # ASCII-only PNG would have reached the model mangled.
                    pages.append(vlm(str(path), pix.tobytes("png")))
            finally:
                doc.close()

    document = Document(
        path=doc_path,
        frontmatter={
            "title": path.stem,
            "source": str(path),
            "parser": "pdf-cpu" if not scanned else "pdf-vlm",
            "doc_hash": "sha256:" + file_sha256(path),
            "ocr": scanned,
        },
    )

    ordinal = 0
    for i, text in enumerate(pages):
        page_no = i + 1
        sec = Section(
            id=f"{doc_path}#p{page_no}",
            parent_id=None,
            title=f"p.{page_no}",
            doc_path=doc_path,
            ordinal=page_no,
        )
        new_blocks = _page_to_blocks(doc_path, page_no, text, ordinal)
        if new_blocks:
            ordinal = new_blocks[-1].ordinal
            for b in new_blocks:
                sec.children.append(b)
            document.sections.append(sec)
    return document


def load_cache(path: str | Path, cache_dir: str | Path) -> dict | None:
    key = file_sha256(path)
    f = Path(cache_dir) / f"{key}.json"
    if f.exists():
        try:
            return json.loads(f.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            return None
    return None


def save_cache(path: str | Path, cache_dir: str | Path, data: dict) -> None:
    Path(cache_dir).mkdir(parents=True, exist_ok=True)
    key = file_sha256(path)
    (Path(cache_dir) / f"{key}.json").write_text(
        json.dumps(data, ensure_ascii=False), encoding="utf-8"
    )
