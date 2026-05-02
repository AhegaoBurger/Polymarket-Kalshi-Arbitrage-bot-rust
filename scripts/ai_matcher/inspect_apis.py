"""Ad-hoc API inspector for the matcher's external endpoints.

Runs from `scripts/ai_matcher/` via:

    uv run python inspect_apis.py <subcommand> [args]

Subcommands hit the live Kalshi and Polymarket APIs the matcher uses, so the
output reflects whatever those services return *right now*. Use it to verify
field presence, tune category aliases, debug ingestion drops, etc.

Examples:

    # Raw responses — pretty-printed JSON
    uv run python inspect_apis.py kalshi-events --limit 5
    uv run python inspect_apis.py kalshi-markets KXELONMARS-99
    uv run python inspect_apis.py poly-markets --limit 5
    uv run python inspect_apis.py poly-market val-dsy-flk-2026-05-03-game2

    # Field-presence summaries — counts how often each field is null/empty
    uv run python inspect_apis.py poly-markets --summary --limit 50
    uv run python inspect_apis.py kalshi-events --summary --limit 100

    # Bucket assignment dry-run — runs our parser + resolve_bucket
    uv run python inspect_apis.py poly-buckets --limit 100
    uv run python inspect_apis.py kalshi-buckets --limit 50
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from pathlib import Path
from typing import Any

import httpx

KALSHI_API_BASE = "https://api.elections.kalshi.com/trade-api/v2"
GAMMA_API_BASE = "https://gamma-api.polymarket.com"
DEFAULT_TIMEOUT = 15.0


# --- Pretty printing helpers ----------------------------------------------

def _print_json(obj: Any) -> None:
    print(json.dumps(obj, indent=2, default=str))


def _summarize_field_presence(records: list[dict], fields: list[str]) -> None:
    """For each field name, count how many of the records have a non-null/non-empty value."""
    n = len(records)
    print(f"Total records: {n}")
    print()
    print(f"{'Field':<32} {'Present':<10} {'Null/Empty':<12} {'Sample value'}")
    print("-" * 100)
    for f in fields:
        present = 0
        sample = None
        for r in records:
            v = r.get(f)
            if v not in (None, "", [], {}):
                present += 1
                if sample is None:
                    sample = v
        sample_str = repr(sample)[:50] if sample is not None else "—"
        print(f"{f:<32} {present:<10} {n - present:<12} {sample_str}")


# --- Kalshi commands ------------------------------------------------------

def cmd_kalshi_events(args: argparse.Namespace) -> None:
    """GET /events?status=open[&cursor=...]"""
    url = f"{KALSHI_API_BASE}/events?limit={args.limit}&status={args.status}"
    if args.cursor:
        url += f"&cursor={args.cursor}"
    print(f"GET {url}", file=sys.stderr)

    with httpx.Client(timeout=DEFAULT_TIMEOUT) as c:
        resp = c.get(url)
        resp.raise_for_status()
        body = resp.json()

    events = body.get("events", []) or []
    cursor = body.get("cursor", "")

    print(f"# Got {len(events)} events; next cursor: {cursor!r}", file=sys.stderr)

    if args.summary:
        fields = ["event_ticker", "title", "sub_title", "category", "status",
                  "series_ticker", "mutually_exclusive", "tags"]
        _summarize_field_presence(events, fields)
        cats = Counter(e.get("category") or "(empty)" for e in events)
        print()
        print("Category distribution:")
        for cat, n in cats.most_common():
            print(f"  {cat!r:<30} {n}")
    else:
        _print_json(events[: args.show])


def cmd_kalshi_markets(args: argparse.Namespace) -> None:
    """GET /markets?event_ticker=X"""
    url = f"{KALSHI_API_BASE}/markets?event_ticker={args.event_ticker}&limit={args.limit}"
    print(f"GET {url}", file=sys.stderr)

    with httpx.Client(timeout=DEFAULT_TIMEOUT) as c:
        resp = c.get(url)
        resp.raise_for_status()
        body = resp.json()

    markets = body.get("markets", []) or []
    print(f"# Got {len(markets)} markets", file=sys.stderr)

    if args.summary:
        fields = ["ticker", "event_ticker", "title", "subtitle", "yes_sub_title",
                  "category", "rules_primary", "close_time", "status", "liquidity",
                  "volume", "open_interest_fp"]
        _summarize_field_presence(markets, fields)
    else:
        _print_json(markets[: args.show])


def cmd_kalshi_buckets(args: argparse.Namespace) -> None:
    """Walk events, run parser, show bucket distribution."""
    from ai_matcher.categories import load_category_config
    from ai_matcher.ingestion import (
        parse_kalshi_event,
        parse_kalshi_markets_response,
    )

    cfg = load_category_config(_repo_root() / "config" / "category_equivalence.json")
    if not cfg.buckets:
        print("WARN: empty category config; everything will bucket as Unknown",
              file=sys.stderr)

    events_url = f"{KALSHI_API_BASE}/events?limit={args.limit}&status=open"
    print(f"GET {events_url}", file=sys.stderr)
    with httpx.Client(timeout=DEFAULT_TIMEOUT) as c:
        ev_body = c.get(events_url).json()
        events = [parse_kalshi_event(e) for e in (ev_body.get("events") or [])]
        events = [e for e in events if e is not None]

        bucket_counts: Counter = Counter()
        category_counts: Counter = Counter()
        total_markets = 0
        unknown_examples: list[tuple[str, str]] = []

        for ev in events[: args.limit]:
            try:
                m_body = c.get(
                    f"{KALSHI_API_BASE}/markets?event_ticker={ev['event_ticker']}&limit=200"
                ).json()
            except httpx.HTTPError as e:
                print(f"  skip {ev['event_ticker']}: {e}", file=sys.stderr)
                continue
            markets, drops = parse_kalshi_markets_response(
                m_body,
                event_title=ev["title"],
                event_category=ev.get("category", "") or "",
                category_config=cfg,
            )
            total_markets += len(markets)
            for m in markets:
                bucket_counts[m.bucket] += 1
                category_counts[m.category or "(empty)"] += 1
                if m.bucket == "Unknown" and len(unknown_examples) < 10:
                    unknown_examples.append((m.ticker, m.category or "(empty)"))

    print(f"# {total_markets} markets across {len(events[: args.limit])} events")
    print()
    print("Bucket distribution:")
    for b, n in bucket_counts.most_common():
        pct = (n / total_markets * 100) if total_markets else 0
        print(f"  {b:<20} {n:<6} ({pct:>5.1f}%)")
    print()
    print("Raw category distribution:")
    for cat, n in category_counts.most_common(20):
        print(f"  {cat!r:<32} {n}")
    if unknown_examples:
        print()
        print("Sample Unknown markets (ticker, category):")
        for t, c in unknown_examples:
            print(f"  {t}  ←  category={c!r}")


# --- Polymarket commands --------------------------------------------------

def cmd_poly_markets(args: argparse.Namespace) -> None:
    """GET /markets?limit=N&offset=N&active=true&closed=false&order=liquidity"""
    url = (
        f"{GAMMA_API_BASE}/markets"
        f"?limit={args.limit}&offset={args.offset}"
        f"&active=true&closed=false"
        f"&order=liquidity&ascending=false"
    )
    print(f"GET {url}", file=sys.stderr)

    with httpx.Client(timeout=DEFAULT_TIMEOUT) as c:
        resp = c.get(url)
        resp.raise_for_status()
        body = resp.json()

    if not isinstance(body, list):
        print("WARN: response was not a list", file=sys.stderr)
        _print_json(body)
        return

    print(f"# Got {len(body)} markets", file=sys.stderr)

    if args.summary:
        fields = ["conditionId", "slug", "question", "description", "category",
                  "tags", "endDate", "endDateIso", "liquidity", "liquidityNum",
                  "volume", "active", "closed", "outcomes", "clobTokenIds",
                  "events", "groupItemTitle"]
        _summarize_field_presence(body, fields)
        # Drill into the embedded events sub-object too, since that's where some
        # metadata sometimes lives:
        embedded_events = []
        for m in body:
            evs = m.get("events") or []
            if evs:
                embedded_events.append(evs[0])
        if embedded_events:
            print()
            print(f"Embedded `events[0]` sub-object presence ({len(embedded_events)} samples):")
            event_fields = ["id", "ticker", "slug", "title", "description",
                            "category", "tags", "endDate", "image",
                            "eventMetadata"]
            _summarize_field_presence(embedded_events, event_fields)
    else:
        _print_json(body[: args.show])


def cmd_poly_market(args: argparse.Namespace) -> None:
    """GET /markets?slug=<slug>"""
    url = f"{GAMMA_API_BASE}/markets?slug={args.slug}"
    print(f"GET {url}", file=sys.stderr)

    with httpx.Client(timeout=DEFAULT_TIMEOUT) as c:
        resp = c.get(url)
        resp.raise_for_status()
        body = resp.json()

    _print_json(body)


def cmd_poly_buckets(args: argparse.Namespace) -> None:
    """Fetch markets, run parser, show bucket distribution."""
    from ai_matcher.categories import load_category_config
    from ai_matcher.ingestion import parse_poly_gamma_markets_response

    cfg = load_category_config(_repo_root() / "config" / "category_equivalence.json")
    if not cfg.buckets:
        print("WARN: empty category config; everything will bucket as Unknown",
              file=sys.stderr)

    url = (
        f"{GAMMA_API_BASE}/markets"
        f"?limit={args.limit}&offset=0"
        f"&active=true&closed=false"
        f"&order=liquidity&ascending=false"
    )
    print(f"GET {url}", file=sys.stderr)
    with httpx.Client(timeout=DEFAULT_TIMEOUT) as c:
        body = c.get(url).json()

    markets, drops = parse_poly_gamma_markets_response(
        body if isinstance(body, list) else [],
        category_config=cfg,
    )

    bucket_counts: Counter = Counter()
    raw_cat_counts: Counter = Counter()
    has_tags = 0
    unknown_examples: list[tuple[str, str, list[str]]] = []
    for m in markets:
        bucket_counts[m.bucket] += 1
        raw_cat_counts[m.category or "(empty)"] += 1
        if m.tags:
            has_tags += 1
        if m.bucket == "Unknown" and len(unknown_examples) < 10:
            unknown_examples.append((m.ticker, m.category or "(empty)", m.tags))

    print(f"# {len(markets)} markets parsed (drops: {drops})")
    print()
    print("Bucket distribution:")
    for b, n in bucket_counts.most_common():
        pct = (n / len(markets) * 100) if markets else 0
        print(f"  {b:<20} {n:<6} ({pct:>5.1f}%)")
    print()
    print(f"Markets with non-empty `tags`: {has_tags} / {len(markets)}")
    print()
    print("Raw category distribution (top 20):")
    for cat, n in raw_cat_counts.most_common(20):
        print(f"  {cat!r:<32} {n}")
    if unknown_examples:
        print()
        print("Sample Unknown markets (slug, category, tags):")
        for slug, cat, tags in unknown_examples:
            print(f"  {slug:<50}  cat={cat!r}, tags={tags}")


# --- Helpers --------------------------------------------------------------

def _repo_root() -> Path:
    """Walk up from this file (.../scripts/ai_matcher/inspect_apis.py) to repo root."""
    return Path(__file__).resolve().parents[2]


# --- Argparse wiring ------------------------------------------------------

def main() -> int:
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)

    # Kalshi /events
    pe = sub.add_parser("kalshi-events", help="GET Kalshi /events")
    pe.add_argument("--limit", type=int, default=20)
    pe.add_argument("--cursor", default="", help="Pagination cursor from previous call")
    pe.add_argument("--status", default="open", help="open|closed|settled (default: open)")
    pe.add_argument("--summary", action="store_true",
                    help="Show field-presence + category distribution instead of raw JSON")
    pe.add_argument("--show", type=int, default=3,
                    help="Number of records to dump (raw mode only; default: 3)")
    pe.set_defaults(func=cmd_kalshi_events)

    # Kalshi /markets
    pm = sub.add_parser("kalshi-markets", help="GET Kalshi /markets?event_ticker=X")
    pm.add_argument("event_ticker", help="Kalshi event ticker, e.g. KXELONMARS-99")
    pm.add_argument("--limit", type=int, default=200)
    pm.add_argument("--summary", action="store_true")
    pm.add_argument("--show", type=int, default=3)
    pm.set_defaults(func=cmd_kalshi_markets)

    # Kalshi bucket distribution
    pkb = sub.add_parser("kalshi-buckets", help="Run parser on N Kalshi events, show bucket distribution")
    pkb.add_argument("--limit", type=int, default=50,
                     help="Number of events to walk (default: 50)")
    pkb.set_defaults(func=cmd_kalshi_buckets)

    # Polymarket /markets list
    pp = sub.add_parser("poly-markets", help="GET Polymarket Gamma /markets")
    pp.add_argument("--limit", type=int, default=20)
    pp.add_argument("--offset", type=int, default=0)
    pp.add_argument("--summary", action="store_true",
                    help="Show field-presence summary instead of raw JSON")
    pp.add_argument("--show", type=int, default=3)
    pp.set_defaults(func=cmd_poly_markets)

    # Polymarket single market
    pp1 = sub.add_parser("poly-market", help="GET Polymarket Gamma /markets?slug=<slug>")
    pp1.add_argument("slug", help="Polymarket market slug")
    pp1.set_defaults(func=cmd_poly_market)

    # Polymarket bucket distribution
    ppb = sub.add_parser("poly-buckets", help="Run parser on N Polymarket markets, show bucket distribution")
    ppb.add_argument("--limit", type=int, default=100)
    ppb.set_defaults(func=cmd_poly_buckets)

    args = p.parse_args()
    args.func(args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
