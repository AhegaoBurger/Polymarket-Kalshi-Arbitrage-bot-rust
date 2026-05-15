"""
Diagnostic: check on-chain balances + token positionId derivation.

When redeemPositions returned payout=0 despite Polymarket's API showing the
deposit wallet holds 8 UP shares, that meant: the tokens exist at the asset
ID Polymarket reports, but they're held at a different (collateralToken,
conditionId, indexSet) tuple than the standard CTF derivation produces.

This script:
  1. Reads the deposit wallet's pUSD balance (sanity check)
  2. Reads the CTF balanceOf for the actual asset ID Polymarket reports
  3. Computes the *expected* positionId for (pUSD, conditionId, indexSet=1)
  4. Compares — if they differ, V2 uses a wrapped collateral, and we need
     to find the wrapper address to use in redeemPositions
"""

import json
import os
import sys
from urllib import request

from dotenv import load_dotenv
from eth_utils import keccak

POLYGON_RPC = "https://polygon-rpc.com/"  # primary (rate-limited free tier; often returns "tenant disabled")
FALLBACK_RPCS = [
    "https://polygon.gateway.tenderly.co",
    "https://polygon-bor-rpc.publicnode.com",
    "https://polygon.api.onfinality.io/public",
    "https://1rpc.io/matic",
    "https://polygon.drpc.org",
]
# urllib's default UA ("Python-urllib/3.13") is blocked by most public RPC Cloudflare WAFs.
RPC_HEADERS = {
    "Content-Type": "application/json",
    "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
}

CTF = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045"
PUSD = "0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB"
DEPOSIT_WALLET = "0x4fd14bE8bAFc4b82A85f7fA8E0862eCb14A7B85A"

# From the polymarket positions API for the winning UP position:
ASSET_ID = 61012642775551976270876265711202970337816029470978197017229811514093980079862
CONDITION_ID_HEX = "c9bacfb85eaeaf1f649f260e663652ec0ddd370f7f85c62198198c41b143ec0a"


def rpc_call(rpc_url: str, payload: dict) -> dict:
    req = request.Request(
        rpc_url,
        data=json.dumps(payload).encode(),
        headers=RPC_HEADERS,
    )
    return json.loads(request.urlopen(req, timeout=15).read())


def first_working_rpc() -> str:
    """Honor POLYGON_RPC_URL if set; otherwise probe public RPCs with a real eth_call,
    not just eth_chainId — some endpoints serve chainId from cache but 403 on eth_call."""
    env_rpc = os.environ.get("POLYGON_RPC_URL")
    if env_rpc:
        return env_rpc
    # pUSD.balanceOf(0x0) — cheap, but exercises the same code path as the real queries.
    probe = {
        "jsonrpc": "2.0", "method": "eth_call", "id": 0,
        "params": [{"to": PUSD, "data": "0x70a08231" + "0" * 64}, "latest"],
    }
    for url in [POLYGON_RPC, *FALLBACK_RPCS]:
        try:
            r = rpc_call(url, probe)
            if isinstance(r.get("result"), str) and r["result"].startswith("0x") and "error" not in r:
                return url
            print(f"  {url}: {r}", file=sys.stderr)
        except Exception as exc:
            print(f"  {url}: {exc}", file=sys.stderr)
    raise RuntimeError(
        "no working Polygon RPC — set POLYGON_RPC_URL in .env "
        "(Alchemy/QuickNode/Infura free tier all work)"
    )


def eth_call(rpc: str, to: str, data: str) -> str:
    payload = {
        "jsonrpc": "2.0", "method": "eth_call", "id": 1,
        "params": [{"to": to, "data": data}, "latest"],
    }
    r = rpc_call(rpc, payload)
    return r.get("result", "0x0")


def compute_positionid(collateral_token_hex: str, condition_id_hex: str, index_set: int) -> int:
    """
    Replicate CTHelpers.getCollectionId + getPositionId for parentCollectionId=0.

    Gnosis CTF v1 (canonical):
      hashV       = uint(keccak256(conditionId || indexSet_uint256)) % (BN256Q - 1)
      collectionId = parentCollectionId XOR keccak256(parentCollectionId || conditionId || hashV)
      positionId   = uint(keccak256(collateralToken || collectionId))

    With parentCollectionId = 0, parentCollectionId XOR x = x, so:
      collectionId = keccak256(zero32 || conditionId || hashV)
    """
    BN256Q = 21888242871839275222246405745257275088696311157297823662689037894645226208583
    condition_id = bytes.fromhex(condition_id_hex.removeprefix("0x"))
    index_set_b32 = index_set.to_bytes(32, "big")
    hash_v = int.from_bytes(keccak(condition_id + index_set_b32), "big") % (BN256Q - 1)
    hash_v_b32 = hash_v.to_bytes(32, "big")

    parent = b"\x00" * 32
    inner = keccak(parent + condition_id + hash_v_b32)
    collection_id = bytes(a ^ b for a, b in zip(parent, inner))

    collateral = bytes.fromhex(collateral_token_hex.removeprefix("0x").lower().rjust(40, "0"))
    # Solidity abi.encodePacked(address, bytes32) = 20 bytes + 32 bytes
    position_id = int.from_bytes(keccak(collateral + collection_id), "big")
    return position_id


def main() -> int:
    load_dotenv(os.path.join(os.path.dirname(__file__), "..", "..", ".env"))
    rpc = first_working_rpc()
    print(f"Using RPC: {rpc}\n")

    deposit_no_prefix = DEPOSIT_WALLET.removeprefix("0x").lower()
    asset_hex = format(ASSET_ID, "064x")

    # 1. pUSD balance
    pusd_calldata = "0x70a08231" + ("0" * 24) + deposit_no_prefix
    pusd_raw = eth_call(rpc, PUSD, pusd_calldata)
    pusd = int(pusd_raw, 16) if pusd_raw and pusd_raw != "0x" else 0
    print(f"pUSD.balanceOf({DEPOSIT_WALLET}):")
    print(f"  ${pusd / 1_000_000:.4f} ({pusd} micros)\n")

    # 2. CTF balanceOf for the polymarket-reported asset ID
    ctf_calldata = "0x00fdd58e" + ("0" * 24) + deposit_no_prefix + asset_hex
    ctf_raw = eth_call(rpc, CTF, ctf_calldata)
    ctf_bal = int(ctf_raw, 16) if ctf_raw and ctf_raw != "0x" else 0
    print(f"CTF.balanceOf({DEPOSIT_WALLET}, polymarket_asset_id):")
    print(f"  asset_id (decimal): {ASSET_ID}")
    print(f"  asset_id (hex)    : 0x{asset_hex}")
    print(f"  → {ctf_bal} tokens (Polymarket API says 8)\n")

    # 3. Computed positionId for (pUSD, conditionId, indexSet=1)
    print("Computed positionId for standard CTF derivation:")
    for index_set, label in [(1, "UP"), (2, "DOWN")]:
        pid = compute_positionid(PUSD, CONDITION_ID_HEX, index_set)
        pid_hex = format(pid, "064x")
        match = "✓ MATCHES" if pid == ASSET_ID else "✗ does not match"
        print(f"  indexSet={index_set} ({label}): 0x{pid_hex} {match}")
        # Also check balance at this positionId
        bal_calldata = "0x00fdd58e" + ("0" * 24) + deposit_no_prefix + pid_hex
        bal_raw = eth_call(rpc, CTF, bal_calldata)
        bal = int(bal_raw, 16) if bal_raw and bal_raw != "0x" else 0
        print(f"    CTF.balanceOf at this positionId: {bal} tokens")

    print()
    print("Interpretation:")
    print("  If polymarket_asset_id matches indexSet=1 computed: standard derivation works,")
    print("    and the redemption args we sent were correct (something else is wrong).")
    print("  If they DON'T match: V2 uses a different collateral token than pUSD when")
    print("    minting CTF outcome tokens. We need to find that wrapper address.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
