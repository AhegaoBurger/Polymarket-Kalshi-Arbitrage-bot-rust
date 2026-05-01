"""Per-category TTL scheduler.

Categories live in `config/ai_categories.json`. Last-run state lives in
`.ai_matcher_schedule.json` at the project root.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path


@dataclass
class Category:
    name: str
    ttl_secs: int


class Scheduler:
    def __init__(self, categories_path: Path, state_path: Path) -> None:
        self.categories_path = categories_path
        self.state_path = state_path
        self._categories: list[Category] = self._load_categories()
        self._last_run: dict[str, int] = self._load_state()

    def _load_categories(self) -> list[Category]:
        if not self.categories_path.exists():
            return []
        data = json.loads(self.categories_path.read_text())
        return [Category(name=c["name"], ttl_secs=int(c["ttl_secs"])) for c in data]

    def _load_state(self) -> dict[str, int]:
        if not self.state_path.exists():
            return {}
        try:
            return {k: int(v) for k, v in json.loads(self.state_path.read_text()).items()}
        except (json.JSONDecodeError, ValueError):
            return {}

    def due_categories(self, now_secs: int) -> list[Category]:
        """Categories whose TTL has elapsed since last run.

        Categories that have never run are unconditionally due — first-start
        of the sidecar always covers everything regardless of TTL length.
        """
        out: list[Category] = []
        for c in self._categories:
            last = self._last_run.get(c.name)
            if last is None or now_secs - last >= c.ttl_secs:
                out.append(c)
        return out

    def mark_ran(self, name: str, now_secs: int) -> None:
        self._last_run[name] = now_secs
        self.state_path.write_text(json.dumps(self._last_run))
