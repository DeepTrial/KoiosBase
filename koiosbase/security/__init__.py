"""ACL / multi-tenancy — filter at retrieval, never after generation (§9.2)."""

from .acl import (  # noqa: F401
    PUBLIC,
    allowed,
    filter_rows,
    grants_for,
    visible_paths,
)
