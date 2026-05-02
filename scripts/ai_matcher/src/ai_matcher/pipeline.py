"""End-to-end run pipeline: ingestion → embedding → retrieval → verification → outputs."""

from __future__ import annotations

import datetime as dt
import json
import os
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np

from collections import defaultdict

from ai_matcher.categories import CategoryConfig
from ai_matcher.ingestion import Ingestion, IngestionResult, Market
from ai_matcher.overrides import OverrideOutcome, OverrideSet
from ai_matcher.report import PairAuditRow, render_report
from ai_matcher.retrieval import BucketedHnswRetrieval
from ai_matcher.verifier import EmbeddingsOnlyVerifier, Verifier


def date_overlap_ok(
    k: Market,
    p: Market,
    cfg: CategoryConfig,
    scale: float,
) -> bool:
    """Return True iff the two markets' UTC expiries are within the bucket's tolerance.

    Bucket selection: the Kalshi-side bucket if known; otherwise the Polymarket
    bucket; otherwise default_tolerance_days. Both Unknown → default_tolerance_days.
    """
    bucket = k.bucket if k.bucket != "Unknown" else p.bucket
    tol_days = (
        cfg.buckets[bucket].tolerance_days
        if bucket in cfg.buckets
        else cfg.default_tolerance_days
    )
    if k.close_time_utc is None or p.close_time_utc is None:
        return False
    delta_seconds = abs((k.close_time_utc - p.close_time_utc).total_seconds())
    return delta_seconds <= tol_days * scale * 86_400


@dataclass
class PipelineConfig:
    project_root: Path
    audit_dir: Path
    matches_path: Path
    audit_log_path: Path
    overrides_path: Path
    embedding_model: str
    llm_model: str
    top_k: int = 8
    min_cosine: float = 0.55
    # Min confidence to accept a pair. LLM verifier defaults to 0.9; embeddings-only
    # mode lowers this so cosine-based confidence isn't filtered by the LLM-tuned floor.
    acceptance_min_confidence: float = 0.9
    category_config: CategoryConfig | None = None
    expiry_tolerance_scale: float = 1.0


def _atomic_write_json(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=path.name + ".", dir=str(path.parent))
    try:
        with os.fdopen(fd, "w") as f:
            json.dump(payload, f, indent=2, sort_keys=True)
        os.replace(tmp_name, path)
    except Exception:
        if os.path.exists(tmp_name):
            os.unlink(tmp_name)
        raise


def _call_verifier(verifier: Any, k: Market, p: Market, cosine: float):
    """Dispatch to the right verify() signature.

    The LLM `Verifier.verify(k, p)` ignores cosine; the `EmbeddingsOnlyVerifier.verify(k, p, cosine)`
    requires it. We use an isinstance check (not duck typing) because MagicMock-based
    test verifiers respond truthfully to any `hasattr` check.
    """
    if isinstance(verifier, EmbeddingsOnlyVerifier):
        return verifier.verify(k, p, cosine)
    return verifier.verify(k, p)


def _kalshi_url(ticker: str) -> str:
    return f"https://kalshi.com/markets/{ticker}"


def _poly_url(slug: str) -> str:
    return f"https://polymarket.com/event/{slug}"


def _override_snippet(k_ticker: str, poly_cid: str) -> str:
    return json.dumps({
        "kalshi_market_ticker": k_ticker,
        "poly_condition_id": poly_cid,
        "reason": "<fill in>",
    })


def run_pipeline(
    cfg: PipelineConfig,
    ingestion: Any,
    embedder: Any,
    verifier: Any,
) -> dict:
    result: IngestionResult = ingestion.fetch_all()

    counters_drops_ingest = {
        "kalshi_missing_date": 0, "poly_missing_date": 0,
        "kalshi_low_volume": 0, "poly_low_liquidity": 0,
    }
    bucketed_counts: dict[str, int] = defaultdict(int)

    polys_by_bucket: dict[str, list[tuple[np.ndarray, str]]] = defaultdict(list)
    all_polys: list[tuple[np.ndarray, str]] = []
    # Key by condition_id for poly markets (unique per market even if ticker collides).
    # Fall back to ticker when condition_id is absent (tests / Kalshi-side entries).
    poly_by_id: dict[str, Market] = {}
    for m in result.poly:
        bucketed_counts[m.bucket] += 1
        vec = embedder.embed(m)
        uid = m.condition_id if m.condition_id else m.ticker
        polys_by_bucket[m.bucket].append((vec, uid))
        all_polys.append((vec, uid))
        poly_by_id[uid] = m

    embedder.flush()

    retrieval = BucketedHnswRetrieval(
        dim=embedder.dim, top_k=cfg.top_k, min_cosine=cfg.min_cosine
    )
    if all_polys:
        retrieval.build(polys_by_bucket, all_polys)

    overrides = OverrideSet.load(cfg.overrides_path)
    rows: list[PairAuditRow] = []
    accepted_pairs: list[dict] = []
    audit_log_lines: list[str] = []

    accepted = 0
    rejected = 0
    candidates_after_retrieval = 0
    drops_at_date_overlap = 0
    verifier_calls = 0
    verifier_cost_usd = 0.0

    for k in result.kalshi:
        bucketed_counts[k.bucket] += 1
        k_vec = embedder.embed(k)
        candidates = retrieval.query(k_vec, k.bucket) if all_polys else []
        for poly_uid, cosine in candidates:
            p = poly_by_id.get(poly_uid)
            if p is None:
                continue
            candidates_after_retrieval += 1

            if cfg.category_config is not None and not date_overlap_ok(
                k, p, cfg.category_config, cfg.expiry_tolerance_scale
            ):
                drops_at_date_overlap += 1
                tol = (
                    cfg.category_config.buckets[k.bucket].tolerance_days
                    if k.bucket in cfg.category_config.buckets
                    else cfg.category_config.default_tolerance_days
                )
                delta_days = (
                    abs((k.close_time_utc - p.close_time_utc).days)
                    if (k.close_time_utc and p.close_time_utc) else None
                )
                audit_log_lines.append(json.dumps({
                    "ts": dt.datetime.now(tz=dt.timezone.utc).isoformat(),
                    "kalshi": k.ticker, "poly": p.condition_id,
                    "decision": "reject", "reject_reason": "expiry-gap",
                    "bucket_kalshi": k.bucket, "bucket_poly": p.bucket,
                    "cosine": round(float(cosine), 4),
                    "delta_days": delta_days, "tolerance_days": tol,
                }))
                rejected += 1
                continue

            verifier_calls += 1
            decision = _call_verifier(verifier, k, p, cosine)
            verifier_cost_usd += getattr(decision, "cost_usd", 0.0) or 0.0
            override = overrides.lookup(k.ticker, p.condition_id)
            ai_accept = decision.is_accepted(min_confidence=cfg.acceptance_min_confidence)
            if override == OverrideOutcome.BLACKLIST:
                final_accepted = False
            elif override == OverrideOutcome.WHITELIST:
                final_accepted = True
            else:
                final_accepted = ai_accept

            if final_accepted:
                accepted += 1
                accepted_pairs.append({
                    "kalshi_market_ticker": k.ticker,
                    "poly_condition_id": p.condition_id,
                    "poly_yes_token": p.poly_yes_token,
                    "poly_no_token": p.poly_no_token,
                    "category": decision.category,
                    "event_type": decision.event_type,
                    "confidence": decision.confidence,
                    "description": f"{k.title} ↔ {p.title}",
                })
            else:
                rejected += 1

            tol_resolved = (
                cfg.category_config.buckets[k.bucket].tolerance_days
                if cfg.category_config and k.bucket in cfg.category_config.buckets
                else (cfg.category_config.default_tolerance_days if cfg.category_config else None)
            )
            delta_days_resolved = (
                abs((k.close_time_utc - p.close_time_utc).days)
                if (k.close_time_utc and p.close_time_utc) else None
            )
            rows.append(PairAuditRow(
                kalshi_ticker=k.ticker, kalshi_title=k.title,
                kalshi_description=k.description, kalshi_resolution=k.resolution_criteria,
                kalshi_outcomes=k.outcomes, kalshi_url=_kalshi_url(k.ticker),
                poly_slug=p.ticker, poly_title=p.title,
                poly_description=p.description, poly_resolution=p.resolution_criteria,
                poly_outcomes=p.outcomes, poly_url=_poly_url(p.ticker),
                decision=decision, accepted=final_accepted,
                override_snippet=_override_snippet(k.ticker, p.condition_id),
                override_outcome=override.value,
                bucket_kalshi=k.bucket, bucket_poly=p.bucket,
                cosine=float(cosine),
                delta_days=delta_days_resolved,
            ))

            audit_log_lines.append(json.dumps({
                "ts": dt.datetime.now(tz=dt.timezone.utc).isoformat(),
                "kalshi": k.ticker, "poly": p.condition_id,
                "decision": "accept" if final_accepted else "reject",
                "reject_reason": None if final_accepted else "verifier",
                "bucket_kalshi": k.bucket, "bucket_poly": p.bucket,
                "cosine": round(float(cosine), 4),
                "delta_days": delta_days_resolved, "tolerance_days": tol_resolved,
                "confidence": decision.confidence,
                "concerns": decision.concerns,
                "reasoning": decision.reasoning,
                "override": override.value,
                "model": getattr(verifier, "model", ""),
                "cost_usd": getattr(decision, "cost_usd", 0.0),
            }))

    payload = {
        "generated_at": dt.datetime.now(tz=dt.timezone.utc).isoformat(),
        "model": cfg.llm_model,
        "embedding_model": cfg.embedding_model,
        "version": 1,
        "pairs": accepted_pairs,
    }
    _atomic_write_json(cfg.matches_path, payload)
    render_report(rows, cfg.audit_dir)

    if audit_log_lines:
        cfg.audit_log_path.parent.mkdir(parents=True, exist_ok=True)
        with cfg.audit_log_path.open("a") as f:
            for line in audit_log_lines:
                f.write(line + "\n")

    return {
        "ingested": {"kalshi": len(result.kalshi), "poly": len(result.poly)},
        "drops_at_ingest": counters_drops_ingest,
        "bucketed": dict(bucketed_counts),
        "candidates_after_retrieval": candidates_after_retrieval,
        "drops_at_date_overlap": drops_at_date_overlap,
        "verifier_calls": verifier_calls,
        "verifier_cache_hits": getattr(verifier, "cache_hits", 0),
        "verifier_cost_usd": round(verifier_cost_usd, 4),
        "accepted": accepted, "rejected": rejected, "rows": len(rows),
    }


def _project_root() -> Path:
    """Walk up from this file (.../scripts/ai_matcher/src/ai_matcher/pipeline.py) to repo root."""
    return Path(__file__).resolve().parents[4]


def _embeddings_only_mode(no_llm_flag: bool) -> bool:
    """Pick mode from CLI flag (priority) or `EMBEDDINGS_ONLY` env var."""
    if no_llm_flag:
        return True
    val = os.environ.get("EMBEDDINGS_ONLY", "").lower()
    return val in ("1", "true", "yes")


def run_pipeline_default(
    loop_mode: bool = False,
    category: str | None = None,
    sample: int | None = None,
    no_llm: bool = False,
) -> int:
    """Construct real components and run once. Used by the CLI.

    With `no_llm=True` (or `EMBEDDINGS_ONLY=1`), skip the Claude verification stage
    and accept pairs purely on embedding cosine similarity. Cheaper but weaker —
    embeddings cluster by topical similarity, not by resolution-criteria identity.
    """
    project_root = _project_root()
    cfg = PipelineConfig(
        project_root=project_root,
        audit_dir=project_root / "audit",
        matches_path=project_root / ".ai_matches.json",
        audit_log_path=project_root / ".ai_matcher_audit.jsonl",
        overrides_path=project_root / "config" / "manual_overrides.json",
        embedding_model="",
        llm_model="",
    )

    from ai_matcher.categories import load_category_config
    cfg.category_config = load_category_config(project_root / "config" / "category_equivalence.json")
    cfg.expiry_tolerance_scale = float(os.environ.get("EXPIRY_TOLERANCE_SCALE", "1.0"))
    if cfg.expiry_tolerance_scale <= 0:
        print("[ai_matcher] EXPIRY_TOLERANCE_SCALE must be > 0; using 1.0")
        cfg.expiry_tolerance_scale = 1.0

    from ai_matcher.embedder import Embedder

    embedder = Embedder(cache_path=project_root / ".ai_matcher_cache.json")
    cfg.embedding_model = embedder.model_name

    if _embeddings_only_mode(no_llm):
        accept_cosine = float(os.environ.get("EMBEDDINGS_ACCEPT_COSINE", "0.85"))
        verifier: Any = EmbeddingsOnlyVerifier(accept_cosine=accept_cosine)
        cfg.llm_model = verifier.model
        # Lower the acceptance floor so cosine-as-confidence isn't filtered by
        # the LLM-tuned 0.9 default. Tune via EMBEDDINGS_ACCEPT_COSINE.
        cfg.acceptance_min_confidence = accept_cosine
        print(
            f"[ai_matcher] embeddings-only mode (cosine threshold={accept_cosine}, "
            "no LLM verification)"
        )
    else:
        model = os.environ.get("LLM_MODEL", "gpt-4.1-mini")
        verifier = Verifier(
            model=model,
            cache_path=project_root / ".ai_matcher_verifier_cache.json",
        )
        cfg.llm_model = verifier.model

    ingestion = Ingestion(category_config=cfg.category_config)

    summary = run_pipeline(cfg, ingestion=ingestion, embedder=embedder, verifier=verifier)
    print(f"[ai_matcher] run complete: {summary}")
    return 0


def review_default() -> int:
    """Open audit/report.html in the default browser."""
    import webbrowser

    project_root = _project_root()
    report = project_root / "audit" / "report.html"
    if not report.exists():
        print(f"[ai_matcher] no report found at {report}; "
              "run `python -m ai_matcher run` first")
        return 1
    webbrowser.open(report.as_uri())
    return 0


def audit_sample_default(sample: int) -> int:
    """Render and open a single-page HTML with N random accepted pairs."""
    import random
    import webbrowser

    project_root = _project_root()
    matches_path = project_root / ".ai_matches.json"
    if not matches_path.exists():
        print(f"[ai_matcher] no .ai_matches.json — "
              "run `python -m ai_matcher run` first")
        return 1
    payload = json.loads(matches_path.read_text())
    pairs = payload.get("pairs", [])
    if not pairs:
        print("[ai_matcher] no accepted pairs to audit")
        return 0
    # Bias the sample toward low-confidence pairs (the most useful spot-checks).
    pairs.sort(key=lambda p: p.get("confidence", 1.0))
    if len(pairs) <= sample:
        chosen = pairs
    else:
        candidate_pool = pairs[: max(sample * 2, sample)]
        chosen = random.sample(candidate_pool, sample)
    out = project_root / "audit" / f"audit-sample-{sample}.html"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(_render_audit_sample(chosen, payload))
    webbrowser.open(out.as_uri())
    print(f"[ai_matcher] wrote {out} with {len(chosen)} pair(s)")
    return 0


def _render_audit_sample(pairs: list[dict], payload: dict) -> str:
    lines = [
        "<!DOCTYPE html><html><body "
        "style='font-family:sans-serif;max-width:1000px;margin:1em auto;'>",
        f"<h1>ai_matcher audit — {len(pairs)} samples (model {payload.get('model')})</h1>",
    ]
    for p in pairs:
        lines.append("<hr>")
        lines.append(f"<h2>{p.get('description', '')}</h2>")
        lines.append(f"<p>Kalshi: <code>{p.get('kalshi_market_ticker')}</code></p>")
        lines.append(f"<p>Polymarket conditionId: <code>{p.get('poly_condition_id')}</code></p>")
        lines.append(
            f"<p>Category: {p.get('category')} — Event: {p.get('event_type')} — "
            f"Confidence: {p.get('confidence')}</p>"
        )
    lines.append("</body></html>")
    return "\n".join(lines)
