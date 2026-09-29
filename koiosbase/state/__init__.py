"""Knowledge state machine and cascading correction (§8.2/§8.3)."""

from .model import (  # noqa: F401
    FILTERED,
    SCHEMA_EXTRA,
    VALID_STATES,
    cascade_retraction,
    disposition_for,
    ensure_tables,
    filter_visible,
    get_block_state,
    is_stale,
    mark_stale,
    referencing_pages,
    set_block_state,
)
