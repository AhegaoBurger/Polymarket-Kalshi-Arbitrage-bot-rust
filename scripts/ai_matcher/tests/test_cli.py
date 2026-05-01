"""Smoke tests for cli.main — ensures argparse wiring is intact."""

from __future__ import annotations

import pytest

from ai_matcher.cli import main


def test_run_subcommand_returns_zero(capsys):
    rc = main(["run"])
    captured = capsys.readouterr()
    assert rc == 0
    assert "[ai_matcher] run" in captured.out


def test_run_subcommand_with_flags(capsys):
    rc = main(["run", "--loop", "--category", "politics", "--sample", "50"])
    captured = capsys.readouterr()
    assert rc == 0
    assert "loop=True" in captured.out
    assert "category=politics" in captured.out
    assert "sample=50" in captured.out


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
