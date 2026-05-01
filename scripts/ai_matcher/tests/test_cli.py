"""Smoke tests for cli.main — ensures argparse wiring is intact.

The `run` subcommand is mocked because it constructs a real SentenceTransformer
+ Anthropic client otherwise.
"""

from __future__ import annotations

from unittest.mock import patch

import pytest

from ai_matcher.cli import main


def test_run_subcommand_returns_zero_via_pipeline():
    with patch("ai_matcher.pipeline.run_pipeline_default", return_value=0) as mock_run:
        rc = main(["run"])
    assert rc == 0
    mock_run.assert_called_once_with(
        loop_mode=False, category=None, sample=None, no_llm=False
    )


def test_run_subcommand_passes_flags_through():
    with patch("ai_matcher.pipeline.run_pipeline_default", return_value=0) as mock_run:
        rc = main(["run", "--loop", "--category", "politics", "--sample", "50"])
    assert rc == 0
    mock_run.assert_called_once_with(
        loop_mode=True, category="politics", sample=50, no_llm=False
    )


def test_run_subcommand_no_llm_flag():
    with patch("ai_matcher.pipeline.run_pipeline_default", return_value=0) as mock_run:
        rc = main(["run", "--no-llm"])
    assert rc == 0
    mock_run.assert_called_once_with(
        loop_mode=False, category=None, sample=None, no_llm=True
    )


def test_audit_subcommand_default_sample():
    with patch("ai_matcher.pipeline.audit_sample_default", return_value=0) as mock_audit:
        rc = main(["audit"])
    assert rc == 0
    mock_audit.assert_called_once_with(20)


def test_audit_subcommand_custom_sample():
    with patch("ai_matcher.pipeline.audit_sample_default", return_value=0) as mock_audit:
        rc = main(["audit", "--sample", "5"])
    assert rc == 0
    mock_audit.assert_called_once_with(5)


def test_review_subcommand():
    with patch("ai_matcher.pipeline.review_default", return_value=0) as mock_review:
        rc = main(["review"])
    assert rc == 0
    mock_review.assert_called_once_with()


def test_no_subcommand_errors():
    with pytest.raises(SystemExit):
        main([])
