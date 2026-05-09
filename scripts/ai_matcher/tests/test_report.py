"""Tests for the audit HTML report."""

from __future__ import annotations

import datetime as dt
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


def test_resolves_at_column_shows_later_date(tmp_path: Path):
    """Resolves column should show the later of the two close times (capital lockup)."""
    row = _row(
        kalshi_close_time=dt.datetime(2026, 11, 8, tzinfo=dt.timezone.utc),
        poly_close_time=dt.datetime(2026, 11, 15, tzinfo=dt.timezone.utc),
    )
    render_report([row], tmp_path)
    html = (tmp_path / "report.html").read_text()
    assert "Resolves" in html  # column header
    assert "2026-11-15" in html  # later of the two
    # Property short-circuits when only one is known
    assert PairAuditRow.resolves_at.fget(row).isoformat().startswith("2026-11-15")


def test_resolves_at_handles_missing_close_times(tmp_path: Path):
    row = _row()  # no close times set
    render_report([row], tmp_path)
    html = (tmp_path / "report.html").read_text()
    # Resolves cell renders an em-dash placeholder, not blank
    assert "—" in html


def test_per_category_filename_strips_path_separators(tmp_path: Path):
    """Categories like 'Corporate Acquisition/Merger' must not become subdirs."""
    row = _row(decision=Decision(
        confidence=0.95, resolution_match=True, concerns=[],
        reasoning="r", category="Corporate Acquisition/Merger", event_type="Other",
        cost_usd=0.0,
    ))
    render_report([row], tmp_path)
    # Slashes in the category name must be sanitized; the file lives flat.
    children = sorted(p.name for p in tmp_path.iterdir())
    assert any(c.startswith("report-by-category-") and "/" not in c for c in children)
    # No accidental subdirectory got created.
    assert not (tmp_path / "report-by-category-corporate acquisition").exists()
