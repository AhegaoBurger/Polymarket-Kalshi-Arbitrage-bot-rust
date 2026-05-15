"""
Background redemption sweep for the gengar deposit wallet.

polymarket.com's UI doesn't recognize the V2 deposit wallet (it only sees the
account's legacy POLY_PROXY), so Polymarket's "auto-redeem" toggle does NOT
fire for our resolved positions — we have to submit `redeemPositions` ourselves.
This sidecar runs alongside the Rust bot, periodically polling the positions
API and redeeming any position with `redeemable=true` (including losing ones,
which burn outcome tokens for $0 and keep the wallet's CTF-inventory tidy).

Operational shape:
    Terminal A:  cargo run -p pmbots-gengar       # the trading bot
    Terminal B:  python redeem_sweep.py           # this sweep loop

The two processes share no state — they coordinate only via the chain.

Configuration:
    REDEEM_SWEEP_INTERVAL_SECS — poll interval in seconds (default 90)
    REDEEM_SWEEP_DRY_RUN       — set to "1" to log without submitting

Stops on Ctrl+C. Safe to restart; redemption of already-redeemed positions
is a no-op (the contract burns zero tokens for zero payout).
"""

import os
import signal
import sys
import time
from urllib import error as urlerror, request
import json

from dotenv import load_dotenv

# Reuse the well-tested redemption primitive — same calldata, same retry loop.
from redeem_position import build_relayer, redeem_condition

POSITIONS_API = "https://data-api.polymarket.com/positions?user={}"
DEFAULT_INTERVAL_SECS = 90


def fetch_positions(deposit_wallet: str) -> list[dict]:
    """GET /positions?user=<addr>. Returns [] on transient API errors so the
    sweep keeps running across blips."""
    url = POSITIONS_API.format(deposit_wallet)
    req = request.Request(url, headers={"User-Agent": "redeem_sweep/1.0"})
    try:
        with request.urlopen(req, timeout=15) as resp:
            return json.loads(resp.read()) or []
    except (urlerror.URLError, json.JSONDecodeError, TimeoutError) as exc:
        print(f"[sweep] positions API error: {exc} — will retry next tick", file=sys.stderr)
        return []


def redeemable_positions(positions: list[dict]) -> list[dict]:
    """Filter to positions the contract will accept for redemption (resolved
    markets). De-dupe by conditionId since binary markets return one row per
    outcome but a single redeemPositions call settles both legs."""
    seen: set[str] = set()
    out: list[dict] = []
    for p in positions:
        if not p.get("redeemable"):
            continue
        cid = p.get("conditionId")
        if not cid or cid in seen:
            continue
        seen.add(cid)
        out.append(p)
    return out


def sweep_once(relayer, deposit_wallet: str, dry_run: bool) -> int:
    """One pass of the loop. Returns number of redemptions attempted."""
    positions = fetch_positions(deposit_wallet)
    targets = redeemable_positions(positions)
    if not targets:
        print(f"[sweep] no redeemable positions ({len(positions)} total open)")
        return 0

    print(f"[sweep] {len(targets)} redeemable conditionId(s) to process")
    for p in targets:
        cid = p["conditionId"]
        title = p.get("title", "?")
        outcome = p.get("outcome", "?")
        size = p.get("size", 0)
        win = p.get("currentValue", 0) > 0
        tag = "WIN" if win else "loss"
        print(f"[sweep]   {tag}  {outcome} x {size}  —  {title[:60]}  ({cid[:10]}…)")
        if dry_run:
            print("[sweep]   DRY_RUN — skipping submit")
            continue
        try:
            result = redeem_condition(cid, relayer, log=lambda m: print(f"[sweep]   {m}"))
            tx = result.get("transactionHash")
            state = result.get("state")
            print(f"[sweep]   ✓ {state}  tx={tx}")
        except Exception as exc:
            print(f"[sweep]   ✗ redeem failed: {exc}", file=sys.stderr)
    return len(targets)


def main() -> int:
    load_dotenv(os.path.join(os.path.dirname(__file__), "..", "..", ".env"))
    interval = int(os.environ.get("REDEEM_SWEEP_INTERVAL_SECS", DEFAULT_INTERVAL_SECS))
    dry_run = os.environ.get("REDEEM_SWEEP_DRY_RUN", "").strip() == "1"

    try:
        relayer = build_relayer(os.environ)
    except ValueError as exc:
        sys.stderr.write(f"{exc}\n")
        return 1

    deposit_wallet = relayer.get_expected_deposit_wallet()
    print(f"[sweep] deposit wallet:  {deposit_wallet}")
    print(f"[sweep] interval:        {interval}s")
    print(f"[sweep] dry run:         {dry_run}")
    print(f"[sweep] starting; Ctrl+C to stop\n")

    # Graceful shutdown — Ctrl+C between ticks should exit, not raise.
    stopping = {"v": False}
    def _stop(_sig, _frame):
        stopping["v"] = True
        print("\n[sweep] shutdown requested — finishing current tick")
    signal.signal(signal.SIGINT, _stop)
    signal.signal(signal.SIGTERM, _stop)

    while not stopping["v"]:
        try:
            sweep_once(relayer, deposit_wallet, dry_run)
        except Exception as exc:
            print(f"[sweep] tick raised: {exc} — continuing", file=sys.stderr)
        if stopping["v"]:
            break
        # Sleep in 1s chunks so Ctrl+C is responsive.
        for _ in range(interval):
            if stopping["v"]:
                break
            time.sleep(1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
