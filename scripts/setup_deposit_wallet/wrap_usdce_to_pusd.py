"""
One-shot: wrap any USDC.e sitting in the deposit wallet into pUSD via the
Polymarket CollateralOnramp. Useful when USDC.e accumulated from past redemptions
that went directly through CTF.redeemPositions (before we routed through the
CtfCollateralAdapter, which now handles wrap-on-redeem atomically).

The onramp converts 1:1 with no fees. After this script, all your capital is
back as pUSD and the CLOB's trading-balance view reflects the full amount.

Usage:
    python wrap_usdce_to_pusd.py            # wrap the deposit wallet's full USDC.e balance
    python wrap_usdce_to_pusd.py <amount>   # wrap exactly <amount> micros

Idempotent: if balance is 0, exits cleanly. Re-approves max if allowance is
insufficient; otherwise just calls wrap().
"""

import os
import sys

from dotenv import load_dotenv

# Reuse primitives from redeem_position.py — same relayer client, same wallet,
# same batch pattern. No duplication of the Solady signing path.
from redeem_position import (
    build_relayer,
    erc20_balance_of,
    USDCE,
    wrap_usdce_to_pusd,
)


def main() -> int:
    load_dotenv(os.path.join(os.path.dirname(__file__), "..", "..", ".env"))
    try:
        relayer = build_relayer(os.environ)
    except ValueError as exc:
        sys.stderr.write(f"{exc}\n")
        return 1

    deposit_wallet = relayer.get_expected_deposit_wallet()
    print(f"Deposit wallet: {deposit_wallet}")

    if len(sys.argv) >= 2:
        amount = int(sys.argv[1])
        print(f"Wrapping (specified): {amount} micros = ${amount/1e6:.4f} USDC.e")
    else:
        amount = erc20_balance_of(USDCE, deposit_wallet)
        print(f"Wrapping (full balance): {amount} micros = ${amount/1e6:.4f} USDC.e")

    if amount == 0:
        print("Nothing to wrap — deposit wallet holds 0 USDC.e.")
        return 0

    print("\nSubmitting WALLET batch via relayer...")
    result = wrap_usdce_to_pusd(amount, relayer)
    tx_hash = result.get("transactionHash")
    state = result.get("state")
    print(f"\n  ✓ Batch state: {state}")
    print(f"  ✓ Tx hash:     {tx_hash}")
    if tx_hash:
        print(f"  ✓ Polygonscan: https://polygonscan.com/tx/{tx_hash}")

    print("\nVerify the wrap landed:")
    print(f"  USDC.e {deposit_wallet[:10]}.. balance should now be 0")
    print(f"  pUSD balance should be up by {amount/1e6:.4f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
