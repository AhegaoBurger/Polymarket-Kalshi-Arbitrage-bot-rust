from __future__ import annotations

from pathlib import Path

from ai_matcher.report import PairAuditRow, render_report
from ai_matcher.verifier import Decision


def mk_row(
    decision: str = "accept",
    confidence: float = 0.97,
    category: str = "Economics",
) -> PairAuditRow:
    return PairAuditRow(
        kalshi_ticker="KXCPIYOY-26APR-B3.0",
        kalshi_title="Kalshi market",
        kalshi_description="kdesc",
        kalshi_resolution="kr",
        kalshi_outcomes=["Yes", "No"],
        kalshi_url="https://kalshi.com/m/k1",
        poly_slug="cpi-may-2026",
        poly_title="Poly market",
        poly_description="pdesc",
        poly_resolution="pr",
        poly_outcomes=["Yes", "No"],
        poly_url="https://polymarket.com/event/poly1",
        decision=Decision(
            confidence=confidence,
            resolution_match=True,
            concerns=[],
            reasoning="x",
            category=category,
            event_type="Cpi",
        ),
        accepted=(decision == "accept"),
        override_snippet='{"kalshi_market_ticker":"KXCPIYOY-26APR-B3.0","poly_condition_id":"0xC"}',
    )


def test_renders_main_report(tmp_path: Path):
    rows = [mk_row("accept"), mk_row("reject", confidence=0.6, category="Politics")]
    render_report(rows, tmp_path)
    main = (tmp_path / "report.html").read_text()
    assert "Kalshi market" in main
    assert "Poly market" in main
    assert "0.97" in main
    assert "Politics" in main


def test_renders_filter_variants(tmp_path: Path):
    rows = [mk_row("accept"), mk_row("reject", confidence=0.6, category="Politics")]
    render_report(rows, tmp_path)
    accepted = (tmp_path / "report-accepted.html").read_text()
    rejected = (tmp_path / "report-rejected.html").read_text()
    assert "0.97" in accepted and "0.6" not in accepted
    assert "0.6" in rejected


def test_whitelist_override_is_visible_in_report(tmp_path: Path):
    """Spec §4.6.4: an auditor must be able to tell from the HTML when a
    whitelist override flipped an LLM rejection to accepted."""
    row = mk_row("accept", confidence=0.6, category="Politics")
    row.override_outcome = "whitelist"
    render_report([row], tmp_path)
    main = (tmp_path / "report.html").read_text()
    assert "WHITELIST OVERRIDE" in main
    assert "Forced accept" in main


def test_blacklist_override_is_visible_in_report(tmp_path: Path):
    row = mk_row("accept", confidence=0.97, category="Economics")
    row.accepted = False
    row.override_outcome = "blacklist"
    render_report([row], tmp_path)
    main = (tmp_path / "report.html").read_text()
    assert "BLACKLIST OVERRIDE" in main
    assert "Forced reject" in main
