"""v0.2: PDF double-tier adapter + page-level provenance + sources/ pages."""

from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest

pymupdf = pytest.importorskip("pymupdf", reason="PyMuPDF required for PDF tests")

from koiosbase.compile.sources import (
    build_questions,
    build_summary,
    render_source_page,
    write_source_pages,
)
from koiosbase.ingest.pipeline import build_index
from koiosbase.parsers.markdown import all_blocks, parse_markdown
from koiosbase.parsers.pdf import (
    extract_pages_cpu,
    file_sha256,
    looks_scanned,
    parse_pdf,
)


@pytest.fixture(scope="module")
def sample_pdf(tmp_path_factory) -> Path:
    """Build a real 3-page PDF so parsing is exercised, not mocked."""
    doc = pymupdf.open()
    for title, body in [
        ("ACME Report", "Revenue was 32 billion yuan in 2024."),
        ("Cash Flow", "Operating cash flow reached 7.5 billion."),
        ("Debt", "Total liabilities were 18 billion."),
    ]:
        page = doc.new_page()
        page.insert_text((72, 720), title)
        page.insert_text((72, 690), body)
    out = tmp_path_factory.mktemp("pdf") / "fin.pdf"
    doc.save(str(out))
    doc.close()
    return out


def test_pdf_text_extraction_and_hash(sample_pdf):
    pages = extract_pages_cpu(sample_pdf)
    assert len(pages) == 3
    assert "Revenue" in pages[0]
    assert file_sha256(sample_pdf).startswith(
        ("0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "a", "b", "c", "d", "e", "f")
    )


def test_scanned_detection():
    assert looks_scanned(["", "", "x"]) is True
    assert looks_scanned(["a" * 60, "b" * 60]) is False


def test_pdf_blocks_carry_page_provenance(sample_pdf):
    doc = parse_pdf(sample_pdf, sample_pdf.parent)
    blocks = all_blocks(doc)
    assert blocks, "PDF produced no blocks"
    for b in blocks:
        assert b.page_range, "page_range missing (§4.1/§6.5)"
        assert b.page_range[0] == b.page_range[1]
    pages = {b.page_range[0] for b in blocks}
    assert pages == {1, 2, 3}, f"expected 3 distinct pages, got {pages}"


def test_body_text_is_not_mistaken_for_heading(sample_pdf):
    """Regression: every line once looked like a heading, yielding 0 blocks."""
    blocks = all_blocks(parse_pdf(sample_pdf, sample_pdf.parent))
    assert len(blocks) >= 3


def test_frontmatter_marks_parser_and_hash(sample_pdf):
    fm = parse_pdf(sample_pdf, sample_pdf.parent).frontmatter
    assert fm["parser"] in {"pdf-cpu", "pdf-vlm"}
    assert fm["doc_hash"].startswith("sha256:")


def test_sources_page_is_rendered_with_citations(tmp_path):
    (tmp_path / "raw").mkdir()
    md = tmp_path / "raw" / "note.md"
    md.write_text(
        "---\ntitle: 季度纪要\n---\n# 业绩概述\n本季度营收增长稳健。\n",
        encoding="utf-8",
    )
    doc = parse_markdown(md, tmp_path / "raw")
    blocks = all_blocks(doc)
    page = render_source_page(doc, blocks, "2026-09-29")
    assert "type: source" in page
    assert "generated: true" in page
    # every derived question points back at a real block id (P6)
    for q in build_questions(doc, blocks):
        assert "(" in q and "#" in q
    assert build_summary(blocks)


def test_ingest_writes_sources_and_pdf_pages(tmp_path, sample_pdf):
    (tmp_path / "raw").mkdir()
    (tmp_path / "raw" / "fin.pdf").write_bytes(sample_pdf.read_bytes())
    (tmp_path / "raw" / "note.md").write_text(
        "---\ntitle: 纪要\n---\n# 概述\n营收增长稳健。\n", encoding="utf-8"
    )
    n = build_index(tmp_path)
    assert n >= 4, "PDF + markdown blocks should both index"
    src = tmp_path / "wiki" / "sources"
    assert src.exists() and any(src.iterdir()), "sources/ pages not generated"
    conn = sqlite3.connect(str(tmp_path / ".index" / "tree.db"))
    rows = conn.execute(
        "SELECT COUNT(*) FROM blocks WHERE page_range IS NOT NULL"
    ).fetchone()[0]
    assert rows >= 3, "PDF page provenance missing from index"


def test_write_source_pages_returns_count(tmp_path):
    (tmp_path / "raw").mkdir()
    p = tmp_path / "raw" / "a.md"
    p.write_text("---\ntitle: A\n---\n# X\ncontent here\n", encoding="utf-8")
    doc = parse_markdown(p, tmp_path / "raw")
    n = write_source_pages(tmp_path, [(doc, all_blocks(doc))], "2026-09-29")
    assert n == 1
    assert (tmp_path / "wiki" / "sources" / "a.md").exists()
