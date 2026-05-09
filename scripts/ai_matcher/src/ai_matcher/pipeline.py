"""End-to-end run pipeline: ingestion → embedding → retrieval → verification → outputs."""

from __future__ import annotations

import asyncio
import datetime as dt
import json
import logging
import os
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np
from tqdm import tqdm

from collections import defaultdict

from ai_matcher.categories import CategoryConfig
from ai_matcher.ingestion import Ingestion, IngestionResult, Market
from ai_matcher.overrides import OverrideOutcome, OverrideSet
from ai_matcher.report import PairAuditRow, render_report
from ai_matcher.retrieval import BucketedHnswRetrieval
from ai_matcher.verifier import (
    DEFAULT_LLM_TIMEOUT_SECONDS,
    Decision,
    EmbeddingsOnlyVerifier,
    Verifier,
)

# Quiet litellm's "Give Feedback / Get Help" boilerplate it logs on every
# internal retry. Real exceptions still surface (logged at ERROR level by
# litellm before bubbling); we only suppress the INFO/WARNING chatter.
logging.getLogger("LiteLLM").setLevel(logging.ERROR)


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


def _batch_embed(embedder: Any, markets: list[Market]) -> list[np.ndarray]:
    """Embed a list of markets, preferring the batched embed_many() if present.
    Falls back to per-market embed() for test mocks that don't implement it."""
    if not markets:
        return []
    embed_many = getattr(embedder, "embed_many", None)
    if callable(embed_many):
        return embed_many(markets)
    return [embedder.embed(m) for m in markets]


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


async def _async_verify_all(
    verifier: Verifier,
    work: list[tuple[Market, Market, float]],
    concurrency: int,
    show_progress: bool,
) -> tuple[list[Decision], list[bool], float]:
    """Run `verifier.averify` on every (kalshi, poly, cosine) triple in parallel.

    A bounded `asyncio.Semaphore` keeps at most `concurrency` calls in flight,
    which respects OpenAI RPM/TPM limits. Cache-hit status is sampled BEFORE
    each call so we can attribute cost only to genuinely new LLM requests.

    Returns (decisions, was_cached_flags, total_new_cost_usd).
    """
    sem = asyncio.Semaphore(concurrency)
    decisions: list[Decision | None] = [None] * len(work)
    was_cached: list[bool] = [verifier.is_cached(k, p) for k, p, _ in work]
    counters = {"acc": 0, "rej": 0, "cached_done": 0, "cost": 0.0}

    # Outer-task watchdog: if litellm's own timeout fails to fire (rare, but
    # has been seen on stalled TLS handshakes), this guarantees the pipeline
    # always reaches bookkeeping. Set slightly higher than the inner timeout.
    inner_timeout = float(os.environ.get(
        "LLM_TIMEOUT_SECONDS", str(DEFAULT_LLM_TIMEOUT_SECONDS)
    ))
    outer_timeout = inner_timeout + 10.0

    pbar = tqdm(
        total=len(work),
        desc="Matching",
        unit="pair",
        disable=not show_progress,
    )

    async def one(i: int, k: Market, p: Market) -> None:
        async with sem:
            try:
                d = await asyncio.wait_for(
                    verifier.averify(k, p), timeout=outer_timeout
                )
            except asyncio.TimeoutError:
                d = Decision(
                    confidence=0.0,
                    resolution_match=False,
                    concerns=[f"verifier-error: outer timeout after {outer_timeout}s"],
                    reasoning="LLM call exceeded watchdog timeout — rejecting and continuing.",
                    category="",
                    event_type="Other",
                    cost_usd=0.0,
                )
        decisions[i] = d
        if was_cached[i]:
            counters["cached_done"] += 1
        else:
            counters["cost"] += float(d.cost_usd or 0.0)
        if d.is_accepted(0.9):
            counters["acc"] += 1
        else:
            counters["rej"] += 1
        pbar.set_postfix(
            acc=counters["acc"],
            rej=counters["rej"],
            cached=counters["cached_done"],
            cost=f"${counters['cost']:.2f}",
            refresh=False,
        )
        pbar.update(1)

    try:
        await asyncio.gather(*(one(i, k, p) for i, (k, p, _) in enumerate(work)))
    finally:
        pbar.close()
        verifier.flush()  # persist any cache writes still buffered in memory

    out = [d for d in decisions if d is not None]
    return out, was_cached, counters["cost"]


def _verify_all(
    verifier: Any,
    work: list[tuple[Market, Market, float]],
    show_progress: bool = True,
) -> tuple[list[Decision], list[bool], float]:
    """Run verification on all work items.

    LLM-backed `Verifier` runs concurrently via `asyncio.gather` (controlled by
    `LLM_CONCURRENCY`, default 20). `EmbeddingsOnlyVerifier` and test mocks run
    sequentially via the existing dispatch.

    The async path drives a tqdm bar with live accept/reject/cached/cost.
    The sync path drives the same bar one item at a time, except for empty work.
    """
    if not work:
        return [], [], 0.0

    if isinstance(verifier, Verifier):
        concurrency = max(1, int(os.environ.get("LLM_CONCURRENCY", "20")))
        return asyncio.run(
            _async_verify_all(verifier, work, concurrency, show_progress)
        )

    decisions: list[Decision] = []
    was_cached: list[bool] = []
    cost = 0.0
    iterator: Any = tqdm(work, desc="Matching", unit="pair", disable=not show_progress)
    for k, p, c in iterator:
        hits_before = getattr(verifier, "cache_hits", 0)
        d = _call_verifier(verifier, k, p, c)
        hit = getattr(verifier, "cache_hits", 0) > hits_before
        if not hit:
            cost += float(getattr(d, "cost_usd", 0.0) or 0.0)
        decisions.append(d)
        was_cached.append(hit)
    return decisions, was_cached, cost


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

    counters_drops_ingest = getattr(ingestion, "last_drops", {
        "kalshi_missing_date": 0, "poly_missing_date": 0,
        "kalshi_low_volume": 0, "poly_low_liquidity": 0,
    })
    bucketed_counts: dict[str, int] = defaultdict(int)

    # Pre-batch all embeddings up front. embed_many batches model.encode() into
    # one call per side (instead of one per market) — orders of magnitude faster
    # on CPU and gives the user a tqdm progress bar so they can see liveness.
    # Falls back to per-market embed() for test mocks that don't implement it.
    print(f"[ai_matcher] embedding {len(result.poly)} Polymarket markets...", flush=True)
    poly_vecs = _batch_embed(embedder, result.poly)
    print(f"[ai_matcher] embedding {len(result.kalshi)} Kalshi markets...", flush=True)
    kalshi_vecs = _batch_embed(embedder, result.kalshi)
    embedder.flush()

    polys_by_bucket: dict[str, list[tuple[np.ndarray, str]]] = defaultdict(list)
    all_polys: list[tuple[np.ndarray, str]] = []
    # Key by condition_id for poly markets (unique per market even if ticker collides).
    # Fall back to ticker when condition_id is absent (tests / Kalshi-side entries).
    poly_by_id: dict[str, Market] = {}
    for m, vec in zip(result.poly, poly_vecs):
        bucketed_counts[m.bucket] += 1
        uid = m.condition_id if m.condition_id else m.ticker
        polys_by_bucket[m.bucket].append((vec, uid))
        all_polys.append((vec, uid))
        poly_by_id[uid] = m

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

    # === Pass 1: walk retrieved candidates, drop date-mismatches, collect work ===
    work: list[tuple[Market, Market, float]] = []
    for k, k_vec in zip(result.kalshi, kalshi_vecs):
        bucketed_counts[k.bucket] += 1
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
                    int(abs((k.close_time_utc - p.close_time_utc).total_seconds()) // 86_400)
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

            work.append((k, p, cosine))

    verifier_calls = len(work)

    # === Pre-flight log: how many calls are about to fly ===
    if isinstance(verifier, Verifier):
        cached_count = sum(1 for k, p, _ in work if verifier.is_cached(k, p))
        new_count = len(work) - cached_count
        concurrency = max(1, int(os.environ.get("LLM_CONCURRENCY", "20")))
        print(
            f"[ai_matcher] verifying {len(work)} candidate pairs "
            f"({cached_count} cached, {new_count} new LLM calls, "
            f"concurrency={concurrency})",
            flush=True,
        )
    elif isinstance(verifier, EmbeddingsOnlyVerifier):
        print(
            f"[ai_matcher] verifying {len(work)} candidate pairs "
            f"(embeddings-only, no LLM calls)",
            flush=True,
        )

    # === Pass 2: run verification (async for LLM, sync for embeddings/mocks) ===
    decisions, was_cached_flags, verifier_cost_usd = _verify_all(verifier, work)

    # === Pass 3: bookkeeping — overrides, audit rows, audit log entries ===
    for (k, p, cosine), decision, _was_cached in zip(work, decisions, was_cached_flags):
        override = overrides.lookup(k.ticker, p.condition_id)
        ai_accept = decision.is_accepted(min_confidence=cfg.acceptance_min_confidence)
        if override == OverrideOutcome.BLACKLIST:
            final_accepted = False
        elif override == OverrideOutcome.WHITELIST:
            final_accepted = True
        else:
            final_accepted = ai_accept

        tol_resolved = (
            cfg.category_config.buckets[k.bucket].tolerance_days
            if cfg.category_config and k.bucket in cfg.category_config.buckets
            else (cfg.category_config.default_tolerance_days if cfg.category_config else None)
        )
        delta_days_resolved = (
            int(abs((k.close_time_utc - p.close_time_utc).total_seconds()) // 86_400)
            if (k.close_time_utc and p.close_time_utc) else None
        )

        resolves_at = None
        if k.close_time_utc and p.close_time_utc:
            resolves_at = max(k.close_time_utc, p.close_time_utc)
        elif k.close_time_utc:
            resolves_at = k.close_time_utc
        elif p.close_time_utc:
            resolves_at = p.close_time_utc

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
                "bucket_kalshi": k.bucket,
                "bucket_poly": p.bucket,
                "cosine": round(float(cosine), 4),
                "delta_days": delta_days_resolved,
                "kalshi_close_time": k.close_time_utc.isoformat() if k.close_time_utc else None,
                "poly_close_time": p.close_time_utc.isoformat() if p.close_time_utc else None,
                "resolves_at": resolves_at.isoformat() if resolves_at else None,
            })
        else:
            rejected += 1
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
            kalshi_close_time=k.close_time_utc,
            poly_close_time=p.close_time_utc,
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
    # Write in order of "most important to keep on crash" → least.
    # The audit log is the only crash-investigation breadcrumb, so persist it
    # before the JSON or HTML so a render bug can't cost us the per-pair record.
    if audit_log_lines:
        cfg.audit_log_path.parent.mkdir(parents=True, exist_ok=True)
        with cfg.audit_log_path.open("a") as f:
            for line in audit_log_lines:
                f.write(line + "\n")

    _atomic_write_json(cfg.matches_path, payload)
    render_report(rows, cfg.audit_dir)

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
    """Render N spot-check pairs using the main report template — sort + filter for free."""
    from importlib.resources import files
    from jinja2 import Environment, FileSystemLoader, select_autoescape

    from ai_matcher.report import PairAuditRow
    from ai_matcher.verifier import Decision

    template_dir = files("ai_matcher").joinpath("templates")
    env = Environment(
        loader=FileSystemLoader(str(template_dir)),
        autoescape=select_autoescape(["html"]),
    )
    tpl = env.get_template("report.html.j2")
    rows = []
    for p in pairs:
        rows.append(PairAuditRow(
            kalshi_ticker=p.get("kalshi_market_ticker", ""),
            kalshi_title="", kalshi_description="", kalshi_resolution="",
            kalshi_outcomes=[], kalshi_url="",
            poly_slug=p.get("poly_condition_id", ""),
            poly_title="", poly_description="", poly_resolution="",
            poly_outcomes=[], poly_url="",
            decision=Decision(
                confidence=float(p.get("confidence", 0.0)),
                resolution_match=True, concerns=[],
                reasoning="", category=p.get("category", ""),
                event_type=p.get("event_type", "Other"),
                cost_usd=0.0,
            ),
            accepted=True, override_snippet="{}", override_outcome="none",
            bucket_kalshi=p.get("bucket_kalshi", "Unknown"),
            bucket_poly=p.get("bucket_poly", "Unknown"),
            cosine=float(p.get("cosine", 0.0)),
            delta_days=p.get("delta_days"),
        ))
    return tpl.render(
        title=f"audit sample — model {payload.get('model')}",
        rows=rows, categories=[],
    )
