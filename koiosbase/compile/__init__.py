"""Compile layer: sources/ (v0.2), entities/concepts (v0.3), quality gate."""

from .entities import (  # noqa: F401
    ENTITY_TEMPLATE,
    Entity,
    affected_concepts,
    affected_entities,
    extract_entities,
    render_entity_page,
    slugify,
    write_entity_pages,
)
from .gate import (  # noqa: F401
    ANSWER_TEMPLATE,
    CONFIDENCE_WEIGHT,
    can_promote,
    edge_weight,
    promote,
    read_confidence,
    write_answer_page,
)
from .pipeline import compile_vault  # noqa: F401
from .sources import (  # noqa: F401
    build_questions,
    build_summary,
    render_source_page,
    write_source_pages,
)
