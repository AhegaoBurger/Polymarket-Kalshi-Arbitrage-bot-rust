"""LiteLLM-backed verifier for candidate market pairs.

Spec: docs/superpowers/specs/2026-05-02-matching-prefilter-and-llm-swap-design.md §4
"""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

import litellm

from ai_matcher.ingestion import Market

DEFAULT_MODEL = "gpt-4.1-mini"

VERIFIER_TOOL = {
    "type": "function",
    "function": {
        "name": "report_match_decision",
        "description": "Report whether two prediction markets resolve to identical outcomes.",
        "parameters": {
            "type": "object",
            "properties": {
                "confidence": {"type": "number", "minimum": 0, "maximum": 1},
                "resolution_match": {"type": "boolean"},
                "concerns": {"type": "array", "items": {"type": "string"}},
                "reasoning": {"type": "string"},
                "category": {"type": "string"},
                "event_type": {"type": "string", "enum": [
                    "Sports", "Fomc", "Cpi", "NfpJobs", "Election", "Other"
                ]},
            },
            "required": ["confidence", "resolution_match", "concerns", "reasoning",
                         "category", "event_type"],
        },
    },
}

SYSTEM_PROMPT = """You are evaluating whether two prediction-market contracts — one on Kalshi,
one on Polymarket — resolve to IDENTICAL outcomes. This is for arbitrage
matching, so false positives cost real money.

Be paranoid about resolution criteria. Two markets that sound similar can
resolve differently on edge cases. Classic traps:
  - Different resolution dates / windows
  - Different data sources (e.g., which exchange's BTC price; which BLS series)
  - Qualitative vs. quantitative thresholds with different cutoffs
  - Scope differences (e.g., "any X" vs. "official X"; "primary" vs. "general")
  - Inclusive vs. exclusive range boundaries
  - Different underliers that share a topic (e.g., target rate vs. effective rate)

Score 1.0 only if you can articulate that both markets resolve YES on exactly
the same set of real-world outcomes. Any meaningful definition gap → confidence
below 0.9 and a clear `concerns[]` entry."""


@dataclass
class Decision:
    confidence: float
    resolution_match: bool
    concerns: list[str]
    reasoning: str
    category: str
    event_type: str
    cost_usd: float = 0.0

    def is_accepted(self, min_confidence: float = 0.9) -> bool:
        return (
            self.resolution_match
            and self.confidence >= min_confidence
            and not self.concerns
        )

    @property
    def accepted(self) -> bool:
        return self.is_accepted(0.9)


def user_prompt(kalshi: Market, poly: Market) -> str:
    return (
        f"Kalshi market:\n  Title: {kalshi.title}\n  Description: {kalshi.description}\n"
        f"  Resolution: {kalshi.resolution_criteria}\n  Outcomes: {kalshi.outcomes}\n\n"
        f"Polymarket market:\n  Title: {poly.title}\n  Description: {poly.description}\n"
        f"  Resolution: {poly.resolution_criteria}\n  Outcomes: {poly.outcomes}\n\n"
        "Do these resolve identically? Score accordingly."
    )


class Verifier:
    def __init__(self, model: str = DEFAULT_MODEL, cache_path: Path | None = None) -> None:
        self.model = model
        self.cache_path = cache_path
        self._cache: dict[str, dict] = self._load_cache()
        self.cache_hits = 0
        self.cache_misses = 0

    def _load_cache(self) -> dict[str, dict]:
        if self.cache_path and self.cache_path.exists():
            try:
                return json.loads(self.cache_path.read_text())
            except json.JSONDecodeError:
                return {}
        return {}

    def _save_cache(self) -> None:
        if self.cache_path is None:
            return
        self.cache_path.write_text(json.dumps(self._cache))

    def _cache_key(self, k: Market, p: Market) -> str:
        return f"{self.model}|{k.content_hash()}|{p.content_hash()}"

    def verify(self, kalshi: Market, poly: Market) -> Decision:
        key = self._cache_key(kalshi, poly)
        if key in self._cache:
            self.cache_hits += 1
            return Decision(**self._cache[key])

        self.cache_misses += 1
        resp = litellm.completion(
            model=self.model,
            messages=[
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user",   "content": user_prompt(kalshi, poly)},
            ],
            tools=[VERIFIER_TOOL],
            tool_choice={"type": "function", "function": {"name": "report_match_decision"}},
            num_retries=3,
        )
        tool_input = self._extract_tool_input(resp)
        cost = float(getattr(resp, "_hidden_params", {}).get("response_cost", 0.0) or 0.0)
        decision = Decision(
            confidence=float(tool_input["confidence"]),
            resolution_match=bool(tool_input["resolution_match"]),
            concerns=list(tool_input.get("concerns", [])),
            reasoning=str(tool_input.get("reasoning", "")),
            category=str(tool_input.get("category", "")),
            event_type=str(tool_input.get("event_type", "Other")),
            cost_usd=cost,
        )
        self._cache[key] = asdict(decision)
        self._save_cache()
        return decision

    @staticmethod
    def _extract_tool_input(resp: Any) -> dict:
        choices = getattr(resp, "choices", None) or []
        for ch in choices:
            msg = getattr(ch, "message", None)
            if msg is None:
                continue
            tool_calls = getattr(msg, "tool_calls", None) or []
            for tc in tool_calls:
                fn = getattr(tc, "function", None)
                if fn is None:
                    continue
                args = getattr(fn, "arguments", None)
                if args:
                    return json.loads(args)
        raise ValueError("LiteLLM response missing tool_calls")


class EmbeddingsOnlyVerifier:
    """Verifier replacement that decides purely on cosine similarity (no LLM)."""

    def __init__(self, accept_cosine: float = 0.85) -> None:
        self.accept_cosine = accept_cosine
        self.model = "embeddings-only"
        self.cache_hits = 0
        self.cache_misses = 0

    def verify(self, kalshi: Market, poly: Market, cosine: float) -> Decision:
        accepted = cosine >= self.accept_cosine
        if accepted:
            concerns: list[str] = []
        else:
            concerns = [
                f"cosine {cosine:.3f} below embeddings-only threshold {self.accept_cosine}"
            ]
        return Decision(
            confidence=cosine,
            resolution_match=accepted,
            concerns=concerns,
            reasoning=(
                f"embeddings-only: cosine={cosine:.4f}, threshold={self.accept_cosine} "
                f"(no LLM verification — embeddings only catch topical similarity, "
                f"not resolution-criteria identity)"
            ),
            category="",
            event_type="Other",
            cost_usd=0.0,
        )
