"""
Redeem a resolved Polymarket position via the relayer.

polymarket.com's UI doesn't recognize the V2 deposit wallet (it only sees the
account's legacy POLY_PROXY), so we can't click "Redeem" there. Instead we
submit a WALLET batch to the relayer that calls
ConditionalTokens.redeemPositions(...) from the deposit wallet directly.

Usage:
    python redeem_position.py <conditionId>

The conditionId comes from the Polymarket positions API:
    GET https://data-api.polymarket.com/positions?user=<deposit_wallet>

Or from the `marketID` field in your trades.csv if gengar logged the trade.

For binary markets (BTC Up/Down, etc.) `indexSets=[1, 2]` is always correct —
it tells the CTF contract "redeem my position in both outcomes for this
condition" and the contract pays out $1.00 per winning share, $0 per loser.

The contract burns ALL outcome tokens you hold for the condition — there's no
partial redemption parameter. Calling this on a market where you hold zero
shares is a no-op (it'll mine but transfer nothing).
"""

import os
import sys
import time

from dotenv import load_dotenv
from eth_abi import encode
from eth_utils import keccak
from py_builder_relayer_client.client import RelayClient
from py_builder_relayer_client.exceptions import RelayerApiException
from py_builder_relayer_client.models import DepositWalletCall, TransactionType
from py_builder_signing_sdk.config import BuilderApiKeyCreds, BuilderConfig

# Polygon mainnet — same as setup_deposit_wallet.py
POLYGON_CHAIN_ID = 137
PUSD = "0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB"
CTF = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045"
ZERO_BYTES32 = b"\x00" * 32


def build_redeem_calldata(condition_id_hex: str) -> str:
    """ABI-encode redeemPositions(address,bytes32,bytes32,uint256[1,2]) -> 0x..."""
    if condition_id_hex.startswith("0x"):
        condition_id_hex = condition_id_hex[2:]
    condition_id = bytes.fromhex(condition_id_hex)
    if len(condition_id) != 32:
        raise ValueError(f"conditionId must be 32 bytes, got {len(condition_id)}")

    selector = keccak(text="redeemPositions(address,bytes32,bytes32,uint256[])")[:4]
    params = encode(
        ["address", "bytes32", "bytes32", "uint256[]"],
        [PUSD, ZERO_BYTES32, condition_id, [1, 2]],
    )
    return "0x" + (selector + params).hex()


def main() -> int:
    if len(sys.argv) != 2:
        sys.stderr.write(f"Usage: {sys.argv[0]} <conditionId>\n")
        sys.stderr.write("Example:\n")
        sys.stderr.write(
            f"  {sys.argv[0]} 0xc9bacfb85eaeaf1f649f260e663652ec0ddd370f7f85c62198198c41b143ec0a\n"
        )
        sys.stderr.write("\nFind your redeemable conditionIds with:\n")
        sys.stderr.write(
            "  curl -s 'https://data-api.polymarket.com/positions?user=<deposit_wallet>'\\\n"
        )
        sys.stderr.write("    | jq '.[] | select(.redeemable) | .conditionId'\n")
        return 1

    condition_id = sys.argv[1]
    load_dotenv(os.path.join(os.path.dirname(__file__), "..", "..", ".env"))

    required = ["GENGAR_PRIVATE_KEY", "BUILDER_API_KEY", "BUILDER_SECRET", "BUILDER_PASS_PHRASE"]
    missing = [v for v in required if not os.environ.get(v)]
    if missing:
        sys.stderr.write(f"Missing env vars: {', '.join(missing)}\n")
        return 1

    relayer_url = os.environ.get("RELAYER_URL", "https://relayer-v2.polymarket.com/")
    if not relayer_url.endswith("/"):
        relayer_url += "/"

    builder_creds = BuilderApiKeyCreds(
        key=os.environ["BUILDER_API_KEY"],
        secret=os.environ["BUILDER_SECRET"],
        passphrase=os.environ["BUILDER_PASS_PHRASE"],
    )
    relayer = RelayClient(
        relayer_url,
        POLYGON_CHAIN_ID,
        os.environ["GENGAR_PRIVATE_KEY"],
        BuilderConfig(local_builder_creds=builder_creds),
    )

    eoa_address = relayer.signer.address()
    deposit_wallet = relayer.get_expected_deposit_wallet()
    print(f"EOA:            {eoa_address}")
    print(f"Deposit wallet: {deposit_wallet}")
    print(f"Condition ID:   {condition_id}")
    print(f"CTF contract:   {CTF}")
    print(f"Collateral:     {PUSD} (pUSD)")

    calldata = build_redeem_calldata(condition_id)
    call = DepositWalletCall(target=CTF, value="0", data=calldata)
    print(f"\nCalldata: {calldata[:80]}...")

    # Same retry pattern as the approvals script: relayer may need a moment
    # to converge after the prior batch, and fresh nonce/deadline per attempt.
    print("\nSubmitting WALLET batch (1 call: redeemPositions)...")
    for attempt in range(1, 7):
        nonce_payload = relayer.get_nonce(eoa_address, TransactionType.WALLET.value)
        nonce = str(nonce_payload["nonce"])
        deadline = str(int(time.time()) + 600)
        print(f"  Attempt {attempt}: nonce={nonce}, deadline={deadline}")
        try:
            resp = relayer.execute_deposit_wallet_batch(
                calls=[call],
                wallet_address=deposit_wallet,
                nonce=nonce,
                deadline=deadline,
            )
            result = resp.wait()
            tx_hash = result.get("transactionHash") if result else None
            state = result.get("state") if result else None
            print(f"\n  ✓ Batch state: {state}")
            print(f"  ✓ Tx hash:     {tx_hash}")
            if tx_hash:
                print(f"  ✓ Polygonscan: https://polygonscan.com/tx/{tx_hash}")
            break
        except RelayerApiException as exc:
            msg = str(exc)
            transient = any(
                p in msg for p in ("not registered", "wallet registry", "not yet indexed", "nonce")
            )
            if not transient or attempt == 6:
                raise
            wait_s = min(2 ** attempt, 30)
            print(f"    Transient error: {msg.splitlines()[0][:120]}")
            print(f"    Waiting {wait_s}s...")
            time.sleep(wait_s)

    print("\n" + "=" * 64)
    print("Redemption submitted.")
    print(f"  Deposit wallet:  {deposit_wallet}")
    print()
    print("Verify the payout landed:")
    print(
        f"  curl -s 'https://data-api.polymarket.com/positions?user={deposit_wallet}' | jq"
    )
    print("  (the redeemed position should be gone, balance up by ~$N per winning share)")
    print("=" * 64)
    return 0


if __name__ == "__main__":
    sys.exit(main())
