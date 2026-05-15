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
PUSD = "0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB"  # user-facing collateral
# CTF outcome tokens are minted against USDC.e (bridged), NOT pUSD. Verified on-chain
# 2026-05-15: keccak(USDC.e || getCollectionId(0, conditionId, 1)) == polymarket asset_id.
# pUSD is a user-facing veneer; the CTF positionId derivation predates the pUSD migration.
USDCE = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174"
CTF = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045"
# Polymarket CollateralOnramp — wrap(asset, recipient, amount). USDC.e → pUSD,
# 1:1 with no fees. Verified contract per Polygonscan.
ONRAMP = "0x93070a847efEf7F70739046A929D47a521F5B8ee"
# CtfCollateralAdapter — the "thin adapter" for pUSD-native CTF actions per
# https://docs.polymarket.com/trading/ctf/redeem. Calling redeemPositions on
# the adapter (instead of CTF directly) yields pUSD payout instead of USDC.e:
# adapter pulls USDC.e from CTF, forwards it to pUSD, pUSD mints 1:1 to sender.
# Same selector and calldata shape as CTF.redeemPositions — first address arg
# is actually ignored by the adapter (verified via three live mainnet txs that
# passed pUSD, USDC.e, and 0x0 respectively — all succeeded).
ADAPTER = "0xAdA100Db00Ca00073811820692005400218FcE1f"
MAX_UINT256 = (1 << 256) - 1
ZERO_BYTES32 = b"\x00" * 32


def build_redeem_calldata(condition_id_hex: str) -> str:
    """ABI-encode redeemPositions(USDC.e, 0, conditionId, [1,2]) -> 0x..."""
    if condition_id_hex.startswith("0x"):
        condition_id_hex = condition_id_hex[2:]
    condition_id = bytes.fromhex(condition_id_hex)
    if len(condition_id) != 32:
        raise ValueError(f"conditionId must be 32 bytes, got {len(condition_id)}")

    selector = keccak(text="redeemPositions(address,bytes32,bytes32,uint256[])")[:4]
    params = encode(
        ["address", "bytes32", "bytes32", "uint256[]"],
        [USDCE, ZERO_BYTES32, condition_id, [1, 2]],
    )
    return "0x" + (selector + params).hex()


def build_approve_calldata(spender: str, amount: int = MAX_UINT256) -> str:
    """ABI-encode ERC20.approve(spender, amount). Idempotent — re-approving is a no-op."""
    selector = keccak(text="approve(address,uint256)")[:4]
    params = encode(["address", "uint256"], [spender, amount])
    return "0x" + (selector + params).hex()


def build_set_approval_for_all_calldata(operator: str, approved: bool = True) -> str:
    """ABI-encode ERC1155.setApprovalForAll(operator, approved). For CTF outcome tokens."""
    selector = keccak(text="setApprovalForAll(address,bool)")[:4]
    params = encode(["address", "bool"], [operator, approved])
    return "0x" + (selector + params).hex()


def ctf_is_approved_for_all(owner: str, operator: str,
                            rpc_url: str = "https://polygon.gateway.tenderly.co") -> bool:
    """Read CTF.isApprovedForAll(owner, operator) via Polygon RPC."""
    import json as _json
    from urllib import request as _req
    selector = keccak(text="isApprovedForAll(address,address)")[:4].hex()
    o = owner.removeprefix("0x").lower().rjust(64, "0")
    op = operator.removeprefix("0x").lower().rjust(64, "0")
    data = "0x" + selector + o + op
    payload = {"jsonrpc": "2.0", "id": 1, "method": "eth_call",
               "params": [{"to": CTF, "data": data}, "latest"]}
    req = _req.Request(rpc_url, data=_json.dumps(payload).encode(),
                       headers={"Content-Type": "application/json",
                                "User-Agent": "Mozilla/5.0"})
    r = _json.loads(_req.urlopen(req, timeout=15).read())
    return int(r.get("result", "0x0"), 16) != 0


def build_wrap_calldata(asset: str, recipient: str, amount: int) -> str:
    """ABI-encode CollateralOnramp.wrap(asset, recipient, amount). USDC.e → pUSD 1:1."""
    selector = keccak(text="wrap(address,address,uint256)")[:4]
    params = encode(["address", "address", "uint256"], [asset, recipient, amount])
    return "0x" + (selector + params).hex()


def erc20_balance_of(token: str, holder: str, rpc_url: str = "https://polygon.gateway.tenderly.co") -> int:
    """Read ERC20.balanceOf via Polygon RPC. Used to check USDC.e balance / allowance."""
    import json as _json
    from urllib import request as _req
    selector = keccak(text="balanceOf(address)")[:4].hex()
    holder_padded = holder.removeprefix("0x").lower().rjust(64, "0")
    data = "0x" + selector + holder_padded
    payload = {"jsonrpc": "2.0", "id": 1, "method": "eth_call",
               "params": [{"to": token, "data": data}, "latest"]}
    req = _req.Request(rpc_url, data=_json.dumps(payload).encode(),
                       headers={"Content-Type": "application/json",
                                "User-Agent": "Mozilla/5.0"})
    r = _json.loads(_req.urlopen(req, timeout=15).read())
    return int(r.get("result", "0x0"), 16)


def erc20_allowance(token: str, owner: str, spender: str,
                    rpc_url: str = "https://polygon.gateway.tenderly.co") -> int:
    """Read ERC20.allowance(owner, spender) via Polygon RPC."""
    import json as _json
    from urllib import request as _req
    selector = keccak(text="allowance(address,address)")[:4].hex()
    o = owner.removeprefix("0x").lower().rjust(64, "0")
    s = spender.removeprefix("0x").lower().rjust(64, "0")
    data = "0x" + selector + o + s
    payload = {"jsonrpc": "2.0", "id": 1, "method": "eth_call",
               "params": [{"to": token, "data": data}, "latest"]}
    req = _req.Request(rpc_url, data=_json.dumps(payload).encode(),
                       headers={"Content-Type": "application/json",
                                "User-Agent": "Mozilla/5.0"})
    r = _json.loads(_req.urlopen(req, timeout=15).read())
    return int(r.get("result", "0x0"), 16)


def wrap_usdce_to_pusd(amount: int, relayer: RelayClient, *, log=print) -> dict:
    """Wrap USDC.e to pUSD via the CollateralOnramp. If the onramp's USDC.e
    allowance is below `amount`, prepend an approve(max) call in the same batch
    so the conversion is atomic from the operator's POV.

    Returns the relayer's wait() result. Raises if amount == 0.
    """
    if amount <= 0:
        raise ValueError(f"wrap_usdce_to_pusd: amount must be > 0, got {amount}")

    eoa_address = relayer.signer.address()
    deposit_wallet = relayer.get_expected_deposit_wallet()
    allowance = erc20_allowance(USDCE, deposit_wallet, ONRAMP)
    log(f"  USDC.e allowance for onramp: {allowance} (need {amount})")

    calls = []
    if allowance < amount:
        log("  → including approve(max) in batch")
        calls.append(DepositWalletCall(
            target=USDCE, value="0",
            data=build_approve_calldata(ONRAMP, MAX_UINT256),
        ))
    calls.append(DepositWalletCall(
        target=ONRAMP, value="0",
        data=build_wrap_calldata(USDCE, deposit_wallet, amount),
    ))

    last_exc: RelayerApiException | None = None
    for attempt in range(1, 7):
        nonce_payload = relayer.get_nonce(eoa_address, TransactionType.WALLET.value)
        nonce = str(nonce_payload["nonce"])
        deadline = str(int(time.time()) + 600)
        log(f"  Attempt {attempt}: nonce={nonce}, calls={len(calls)}")
        try:
            resp = relayer.execute_deposit_wallet_batch(
                calls=calls, wallet_address=deposit_wallet,
                nonce=nonce, deadline=deadline,
            )
            return resp.wait() or {}
        except RelayerApiException as exc:
            last_exc = exc
            msg = str(exc)
            transient = any(
                p in msg for p in ("not registered", "wallet registry", "not yet indexed", "nonce")
            )
            if not transient or attempt == 6:
                raise
            wait_s = min(2 ** attempt, 30)
            log(f"    Transient error: {msg.splitlines()[0][:120]}; waiting {wait_s}s")
            time.sleep(wait_s)
    if last_exc is not None:
        raise last_exc
    return {}


def build_relayer(env: dict) -> RelayClient:
    """Construct a RelayClient from env vars. Raises ValueError on missing creds."""
    required = ["GENGAR_PRIVATE_KEY", "BUILDER_API_KEY", "BUILDER_SECRET", "BUILDER_PASS_PHRASE"]
    missing = [v for v in required if not env.get(v)]
    if missing:
        raise ValueError(f"Missing env vars: {', '.join(missing)}")
    relayer_url = env.get("RELAYER_URL", "https://relayer-v2.polymarket.com/")
    if not relayer_url.endswith("/"):
        relayer_url += "/"
    builder_creds = BuilderApiKeyCreds(
        key=env["BUILDER_API_KEY"],
        secret=env["BUILDER_SECRET"],
        passphrase=env["BUILDER_PASS_PHRASE"],
    )
    return RelayClient(
        relayer_url, POLYGON_CHAIN_ID, env["GENGAR_PRIVATE_KEY"],
        BuilderConfig(local_builder_creds=builder_creds),
    )


def redeem_condition(condition_id: str, relayer: RelayClient, *, log=print) -> dict:
    """Submit a redeemPositions WALLET batch for a single conditionId via the
    CtfCollateralAdapter, so payout lands as pUSD (not USDC.e). Returns the
    relayer's wait() result. Retries transient errors up to 6 times.

    The adapter requires a one-time CTF.setApprovalForAll(adapter, true) — if
    not set, we prepend that call to the same batch so the conversion is
    atomic from the operator's POV. Re-running after the approval is in place
    skips the prepend and just sends the redeem call alone.

    The adapter burns ALL outcome tokens the deposit wallet holds for this
    condition — losing positions are no-ops (burn-for-$0) and safe to call.
    """
    eoa_address = relayer.signer.address()
    deposit_wallet = relayer.get_expected_deposit_wallet()

    calls = []
    if not ctf_is_approved_for_all(deposit_wallet, ADAPTER):
        log("  CTF not yet approved for adapter → including setApprovalForAll(true)")
        calls.append(DepositWalletCall(
            target=CTF, value="0",
            data=build_set_approval_for_all_calldata(ADAPTER, True),
        ))
    calls.append(DepositWalletCall(
        target=ADAPTER, value="0",
        data=build_redeem_calldata(condition_id),
    ))

    last_exc: RelayerApiException | None = None
    for attempt in range(1, 7):
        nonce_payload = relayer.get_nonce(eoa_address, TransactionType.WALLET.value)
        nonce = str(nonce_payload["nonce"])
        deadline = str(int(time.time()) + 600)
        log(f"  Attempt {attempt} on {condition_id[:10]}…: nonce={nonce}  calls={len(calls)}")
        try:
            resp = relayer.execute_deposit_wallet_batch(
                calls=calls, wallet_address=deposit_wallet,
                nonce=nonce, deadline=deadline,
            )
            return resp.wait() or {}
        except RelayerApiException as exc:
            last_exc = exc
            msg = str(exc)
            transient = any(
                p in msg for p in ("not registered", "wallet registry", "not yet indexed", "nonce")
            )
            if not transient or attempt == 6:
                raise
            wait_s = min(2 ** attempt, 30)
            log(f"    Transient error: {msg.splitlines()[0][:120]}; waiting {wait_s}s")
            time.sleep(wait_s)
    if last_exc is not None:
        raise last_exc
    return {}


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

    try:
        relayer = build_relayer(os.environ)
    except ValueError as exc:
        sys.stderr.write(f"{exc}\n")
        return 1

    deposit_wallet = relayer.get_expected_deposit_wallet()
    print(f"EOA:            {relayer.signer.address()}")
    print(f"Deposit wallet: {deposit_wallet}")
    print(f"Condition ID:   {condition_id}")
    print(f"Adapter:        {ADAPTER} (CtfCollateralAdapter — payout in pUSD)")
    print(f"CTF contract:   {CTF}")
    print("\nSubmitting WALLET batch (1 call: redeemPositions)...")
    result = redeem_condition(condition_id, relayer)
    tx_hash = result.get("transactionHash")
    state = result.get("state")
    print(f"\n  ✓ Batch state: {state}")
    print(f"  ✓ Tx hash:     {tx_hash}")
    if tx_hash:
        print(f"  ✓ Polygonscan: https://polygonscan.com/tx/{tx_hash}")

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
