"""Category equivalence config loading + bucket resolution.

Spec: docs/superpowers/specs/2026-05-02-matching-prefilter-and-llm-swap-design.md §1
"""

from __future__ import annotations

import json
import logging
from dataclasses import dataclass, field
from pathlib import Path

logger = logging.getLogger(__name__)


@dataclass
class BucketDef:
    """One bucket: which Kalshi/Poly category strings map to it, and the date tolerance."""
    kalshi: list[str] = field(default_factory=list)
    poly: list[str] = field(default_factory=list)
    tolerance_days: int = 30


@dataclass
class CategoryConfig:
    buckets: dict[str, BucketDef] = field(default_factory=dict)
    default_tolerance_days: int = 30


def load_category_config(path: Path) -> CategoryConfig:
    """Load the category equivalence JSON at `path`.

    Missing file → empty config (documented rollback path; logged at INFO).
    Malformed JSON or per-bucket shape errors → bucket(s) skipped (WARNING).
    """
    if not path.exists():
        logger.info("category_equivalence config not found at %s; prefilter disabled", path)
        return CategoryConfig()
    try:
        raw = json.loads(path.read_text())
    except json.JSONDecodeError as e:
        logger.warning("category_equivalence config malformed (%s); prefilter disabled", e)
        return CategoryConfig()

    buckets: dict[str, BucketDef] = {}
    for name, cfg in (raw.get("buckets") or {}).items():
        try:
            buckets[name] = BucketDef(
                kalshi=list(cfg.get("kalshi") or []),
                poly=list(cfg.get("poly") or []),
                tolerance_days=int(cfg.get("tolerance_days", 30)),
            )
        except (AttributeError, TypeError, ValueError) as e:
            logger.warning("category_equivalence bucket '%s' skipped: %s", name, e)
            continue
    default_tol = int(raw.get("default_tolerance_days", 30))
    return CategoryConfig(buckets=buckets, default_tolerance_days=default_tol)
