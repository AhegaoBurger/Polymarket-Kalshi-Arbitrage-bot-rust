"""Command-line dispatcher for the ai_matcher sidecar."""

from __future__ import annotations

import argparse


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="ai_matcher", description="AI matcher sidecar")
    sub = p.add_subparsers(dest="command", required=True)

    run = sub.add_parser("run", help="One discovery pass")
    run.add_argument("--loop", dest="loop_mode", action="store_true",
                     help="Loop with per-category TTLs")
    run.add_argument("--category", help="Restrict to a single category")
    run.add_argument("--sample", type=int, help="Cap sample size per category")

    sub.add_parser("review", help="Open audit/report.html")
    audit = sub.add_parser("audit", help="Random spot-check accepted pairs")
    audit.add_argument("--sample", type=int, default=20)

    sub.add_parser("calibrate-fees", help="One-shot feeSchedule survey")
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.command == "run":
        print(f"[ai_matcher] run (loop={args.loop_mode}, "
              f"category={args.category}, sample={args.sample})")
        return 0
    if args.command == "review":
        print("[ai_matcher] review — not yet wired (Task 11)")
        return 0
    if args.command == "audit":
        print(f"[ai_matcher] audit --sample {args.sample} — not yet wired (Task 11)")
        return 0
    if args.command == "calibrate-fees":
        print("[ai_matcher] calibrate-fees — not yet wired")
        return 0
    return 0
