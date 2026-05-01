"""Static HTML audit report generator (Jinja2)."""

from __future__ import annotations

from dataclasses import dataclass
from importlib.resources import files
from pathlib import Path

from jinja2 import Environment, FileSystemLoader, select_autoescape

from ai_matcher.verifier import Decision


@dataclass
class PairAuditRow:
    kalshi_ticker: str
    kalshi_title: str
    kalshi_description: str
    kalshi_resolution: str
    kalshi_outcomes: list[str]
    kalshi_url: str
    poly_slug: str
    poly_title: str
    poly_description: str
    poly_resolution: str
    poly_outcomes: list[str]
    poly_url: str
    decision: Decision
    accepted: bool
    override_snippet: str
    # "none" | "whitelist" | "blacklist" — surfaced visually so the auditor
    # can tell at a glance when manual_overrides.json flipped the LLM verdict.
    override_outcome: str = "none"


def _env() -> Environment:
    template_dir = files("ai_matcher").joinpath("templates")
    return Environment(
        loader=FileSystemLoader(str(template_dir)),
        autoescape=select_autoescape(["html"]),
    )


def render_report(rows: list[PairAuditRow], out_dir: Path) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    env = _env()
    tpl = env.get_template("report.html.j2")
    categories = sorted({r.decision.category for r in rows if r.decision.category})

    def render_to(name: str, title: str, subset: list[PairAuditRow]) -> None:
        (out_dir / name).write_text(
            tpl.render(title=title, rows=subset, categories=categories)
        )

    render_to("report.html", "ai_matcher — all pairs", rows)
    render_to("report-accepted.html", "ai_matcher — accepted",
              [r for r in rows if r.accepted])
    render_to("report-rejected.html", "ai_matcher — rejected",
              [r for r in rows if not r.accepted])
    for cat in categories:
        render_to(
            f"report-by-category-{cat.lower()}.html",
            f"ai_matcher — {cat}",
            [r for r in rows if r.decision.category == cat],
        )
