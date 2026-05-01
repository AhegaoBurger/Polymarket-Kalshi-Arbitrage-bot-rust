from __future__ import annotations

import json
from pathlib import Path

from ai_matcher.scheduler import Scheduler


def test_scheduler_returns_all_categories_on_first_run(tmp_path: Path):
    cfg = tmp_path / "cats.json"
    cfg.write_text(json.dumps([
        {"name": "politics", "ttl_secs": 7200},
        {"name": "crypto", "ttl_secs": 900},
    ]))
    state = tmp_path / "sched.json"
    s = Scheduler(categories_path=cfg, state_path=state)
    due = s.due_categories(now_secs=1000)
    assert {c.name for c in due} == {"politics", "crypto"}


def test_scheduler_skips_recent_runs(tmp_path: Path):
    cfg = tmp_path / "cats.json"
    cfg.write_text(json.dumps([
        {"name": "politics", "ttl_secs": 100},
        {"name": "crypto", "ttl_secs": 100},
    ]))
    state = tmp_path / "sched.json"
    state.write_text(json.dumps({"politics": 950}))
    s = Scheduler(categories_path=cfg, state_path=state)
    due = s.due_categories(now_secs=1000)
    # politics ran at 950, ttl 100 → next eligible at 1050. Not due at 1000.
    assert {c.name for c in due} == {"crypto"}


def test_scheduler_marks_run_persists_state(tmp_path: Path):
    cfg = tmp_path / "cats.json"
    cfg.write_text(json.dumps([{"name": "politics", "ttl_secs": 100}]))
    state = tmp_path / "sched.json"
    s = Scheduler(categories_path=cfg, state_path=state)
    s.mark_ran("politics", now_secs=2000)
    s2 = Scheduler(categories_path=cfg, state_path=state)
    due = s2.due_categories(now_secs=2050)
    assert due == []
