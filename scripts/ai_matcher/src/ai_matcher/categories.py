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


def resolve_bucket(
    config: CategoryConfig,
    *,
    platform: str,            # "kalshi" or "polymarket"
    category: str,
    tags: list[str],
) -> str:
    """Resolve a market's platform-specific category (and Polymarket tags) to a bucket name.

    Returns the bucket name (e.g., "Politics") or "Unknown" if no bucket matches.
    Case-insensitive, whitespace-trimmed. Polymarket falls back to tags when category is empty;
    Kalshi does not (Kalshi tags are folded into category upstream).
    """
    candidates: list[str] = []
    if category:
        candidates.append(category)
    if platform == "polymarket" and tags:
        candidates.extend(tags)
    candidates = [c.strip().lower() for c in candidates if c and c.strip()]
    if not candidates:
        return "Unknown"

    for bucket_name, bucket_def in config.buckets.items():
        platform_aliases = bucket_def.kalshi if platform == "kalshi" else bucket_def.poly
        aliases_lc = [a.strip().lower() for a in platform_aliases]
        if any(c in aliases_lc for c in candidates):
            return bucket_name
    return "Unknown"
