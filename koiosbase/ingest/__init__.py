"""Ingest pipeline."""

from .pipeline import (  # noqa: F401
    UNIVERSAL_ADAPTERS,
    build_index,
    cmd_ingest,
    cmd_init,
    iter_docs,
    parse_any,
    vault_paths,
)
