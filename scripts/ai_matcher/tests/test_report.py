"""Tests for the audit HTML report."""

from __future__ import annotations

from pathlib import Path

from ai_matcher.report import PairAuditRow, render_report
from ai_matcher.verifier import Decision


def _row(**kwargs) -> PairAuditRow:
    base = dict(
        kalshi_ticker="K", kalshi_title="kt", kalshi_description="kd",
        kalshi_resolution="kr", kalshi_outcomes=["yes", "no"],
        kalshi_url="https://k.example",
        poly_slug="p", poly_title="pt", poly_description="pd",
        poly_resolution="pr", poly_outcomes=["yes", "no"],
        poly_url="https://p.example",
        decision=Decision(
            confidence=0.95, resolution_match=True, concerns=[],
            reasoning="r", category="Politics", event_type="Election",
            cost_usd=0.0007,
        ),
        accepted=True, override_snippet="{}", override_outcome="none",
        bucket_kalshi="Politics", bucket_poly="Politics",
        cosine=0.83, delta_days=12.0,
    )
    base.update(kwargs)
    return PairAuditRow(**base)


def test_report_contains_new_columns(tmp_path: Path):
    render_report([_row()], tmp_path)
    html = (tmp_path / "report.html").read_text()
    assert "Bucket pair" in html
    assert "Cosine" in html
    assert "Δdays" in html
    assert "Politics → Politics" in html
    assert "0.830" in html or "0.83" in html
    assert "12" in html


def test_report_has_sortable_headers(tmp_path: Path):
    render_report([_row()], tmp_path)
    html = (tmp_path / "report.html").read_text()
    assert 'data-sort="numeric"' in html
    assert 'data-sort="string"' in html


def test_report_has_filter_input_and_sticky_header(tmp_path: Path):
    render_report([_row()], tmp_path)
    html = (tmp_path / "report.html").read_text()
    assert 'id="filter"' in html
    assert "position: sticky" in html


def test_report_has_sort_filter_js(tmp_path: Path):
    render_report([_row()], tmp_path)
    html = (tmp_path / "report.html").read_text()
    assert "addEventListener('click'" in html
    assert "addEventListener('input'" in html
