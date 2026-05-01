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
    mock_run.assert_called_once_with(loop_mode=False, category=None, sample=None)


def test_run_subcommand_passes_flags_through():
    with patch("ai_matcher.pipeline.run_pipeline_default", return_value=0) as mock_run:
        rc = main(["run", "--loop", "--category", "politics", "--sample", "50"])
    assert rc == 0
    mock_run.assert_called_once_with(loop_mode=True, category="politics", sample=50)


def test_audit_subcommand_default_sample(capsys):
    rc = main(["audit"])
    captured = capsys.readouterr()
    assert rc == 0
    assert "--sample 20" in captured.out


def test_review_subcommand(capsys):
    rc = main(["review"])
    assert rc == 0


def test_no_subcommand_errors():
    with pytest.raises(SystemExit):
        main([])
