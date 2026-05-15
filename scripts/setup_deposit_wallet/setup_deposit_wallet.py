"""
One-shot setup for the Polymarket V2 deposit wallet flow.

Deploys (if needed) the deposit wallet for your EOA, then submits a single
WALLET batch with the approvals required to trade pUSD<->CTF tokens via the
V2 CTF Exchange (standard + neg-risk variants).

After running this:
  1. Note the printed deposit wallet address.
  2. Transfer your pUSD into that address (e.g., via the polymarket.com UI
     Withdraw + manual Polygon transfer, or via an on-chain pUSD `transfer`
     from wherever the funds currently sit).
  3. Run gengar with GENGAR_SAFE_ADDRESS=<deposit_wallet> and
     GENGAR_SIG_TYPE=3. The Rust CLOB layer handles the rest.

Required environment variables:
  GENGAR_PRIVATE_KEY     EOA private key (the one signing for the deposit wallet)
  BUILDER_API_KEY        From your Polymarket builder portal
  BUILDER_SECRET         From your Polymarket builder portal
  BUILDER_PASS_PHRASE    From your Polymarket builder portal
  RELAYER_URL            Production relayer URL from your builder portal
                         (default fallback: https://relayer-v2.polymarket.com/)

This script is one-shot. The deposit wallet is deterministic per EOA so
re-running won't deploy a second one; it will just re-submit approvals
(which is idempotent — approving max twice is a no-op).
"""

import os
import sys
import time

from dotenv import load_dotenv
from eth_abi import encode
from eth_utils import keccak
from py_builder_relayer_client.client import RelayClient
from py_builder_relayer_client.models import DepositWalletCall, TransactionType
from py_builder_signing_sdk.config import BuilderApiKeyCreds, BuilderConfig

# ---------------------------------------------------------------------------
# Polygon mainnet V2 contract addresses (Polymarket official, sourced from
# rs-clob-client-v2/src/lib.rs:60-91).
# ---------------------------------------------------------------------------
POLYGON_CHAIN_ID = 137
PUSD = "0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB"
CTF = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045"
V2_EXCHANGE_STANDARD = "0xE111180000d2663C0091e4f400237545B87B996B"
V2_EXCHANGE_NEG_RISK = "0xe2222d279d744050d28e00520010520000310F59"
NEG_RISK_ADAPTER = "0xd91E80cF2E7be2e162c6513ceD06f1dD0dA35296"

MAX_UINT256 = (1 << 256) - 1


def erc20_approve_calldata(spender: str, amount: int = MAX_UINT256) -> str:
    """ABI-encode approve(address spender, uint256 amount) -> 0x... hex."""
    selector = keccak(text="approve(address,uint256)")[:4]
    params = encode(["address", "uint256"], [spender, amount])
    return "0x" + (selector + params).hex()


def erc1155_set_approval_for_all_calldata(operator: str, approved: bool = True) -> str:
    """ABI-encode setApprovalForAll(address operator, bool approved) -> 0x... hex."""
    selector = keccak(text="setApprovalForAll(address,bool)")[:4]
    params = encode(["address", "bool"], [operator, approved])
    return "0x" + (selector + params).hex()


def main() -> int:
    # Load .env from the workspace root (one directory up from scripts/).
    load_dotenv(os.path.join(os.path.dirname(__file__), "..", "..", ".env"))

    # Validate required env vars upfront — fail loud with a useful message.
    required = [
        "GENGAR_PRIVATE_KEY",
        "BUILDER_API_KEY",
        "BUILDER_SECRET",
        "BUILDER_PASS_PHRASE",
    ]
    missing = [v for v in required if not os.environ.get(v)]
    if missing:
        sys.stderr.write(f"Missing required env vars: {', '.join(missing)}\n")
        sys.stderr.write("Set them in .env (workspace root) and re-run.\n")
        return 1

    relayer_url = os.environ.get("RELAYER_URL", "https://relayer-v2.polymarket.com/")
    if not relayer_url.endswith("/"):
        relayer_url += "/"

    print(f"Relayer URL: {relayer_url}")
    print(f"Chain ID:    {POLYGON_CHAIN_ID} (Polygon mainnet)")

    builder_creds = BuilderApiKeyCreds(
        key=os.environ["BUILDER_API_KEY"],
        secret=os.environ["BUILDER_SECRET"],
        passphrase=os.environ["BUILDER_PASS_PHRASE"],
    )
    builder_config = BuilderConfig(local_builder_creds=builder_creds)

    relayer = RelayClient(
        relayer_url,
        POLYGON_CHAIN_ID,
        os.environ["GENGAR_PRIVATE_KEY"],
        builder_config,
    )

    eoa_address = relayer.signer.address()
    print(f"EOA address: {eoa_address}")

    # ----------------------------------------------------------------------
    # Step 1: Compute the deterministic deposit wallet address for this EOA.
    # CREATE2(factory, keccak(abi.encode(factory, bytes32(eoa))), initCodeHash)
    # — the relayer client knows the chain's factory + init code constants.
    # ----------------------------------------------------------------------
    deposit_wallet = relayer.get_expected_deposit_wallet()
    print(f"Expected deposit wallet (CREATE2-derived): {deposit_wallet}")

    # ----------------------------------------------------------------------
    # Step 2: Deploy the deposit wallet. Idempotent — if it's already on-chain
    # the relayer will accept or no-op the transaction. Polymarket auto-deploys
    # the wallet for new UI signups, so this may be a no-op for you.
    # ----------------------------------------------------------------------
    print("\n[1/2] Submitting WALLET-CREATE...")
    try:
        deploy_resp = relayer.deploy_deposit_wallet()
        deploy_result = deploy_resp.wait()
        print(f"      Deploy result: {deploy_result}")
    except Exception as exc:
        print(f"      Deploy raised: {exc}")
        print("      (continuing — this is fine if the wallet was already deployed)")

    # ----------------------------------------------------------------------
    # Step 3: Approve the V2 CTF Exchange contracts (standard + neg-risk +
    # neg-risk adapter) to spend pUSD and transfer CTF tokens from the
    # deposit wallet. Bundled into a single WALLET batch.
    #
    # Standard V2 Exchange handles BTC Up/Down (and most binary markets).
    # Neg-risk V2 + adapter only matter for multi-outcome markets (sports
    # finals, election winners). Approving all three now means you don't
    # need to re-run this if you ever expand to neg-risk markets later;
    # approve-max is just a token allowance, no funds move.
    # ----------------------------------------------------------------------
    calls = [
        DepositWalletCall(
            target=PUSD,
            value="0",
            data=erc20_approve_calldata(V2_EXCHANGE_STANDARD),
        ),
        DepositWalletCall(
            target=PUSD,
            value="0",
            data=erc20_approve_calldata(V2_EXCHANGE_NEG_RISK),
        ),
        DepositWalletCall(
            target=PUSD,
            value="0",
            data=erc20_approve_calldata(NEG_RISK_ADAPTER),
        ),
        DepositWalletCall(
            target=CTF,
            value="0",
            data=erc1155_set_approval_for_all_calldata(V2_EXCHANGE_STANDARD),
        ),
        DepositWalletCall(
            target=CTF,
            value="0",
            data=erc1155_set_approval_for_all_calldata(V2_EXCHANGE_NEG_RISK),
        ),
        DepositWalletCall(
            target=CTF,
            value="0",
            data=erc1155_set_approval_for_all_calldata(NEG_RISK_ADAPTER),
        ),
    ]

    # After WALLET-CREATE returns STATE_MINED, the relayer's internal wallet
    # registry has a brief indexing lag before it acknowledges the new wallet
    # for batch submissions. Retry with backoff to wait it out.
    print(f"\n[2/2] Submitting WALLET batch with {len(calls)} approvals...")

    from py_builder_relayer_client.exceptions import RelayerApiException

    batch_result = None
    for attempt in range(1, 7):  # up to 6 attempts = ~63s total wait
        # Fresh nonce + deadline each attempt; the deadline only needs to be
        # in the future at the time the relayer processes it.
        nonce_payload = relayer.get_nonce(eoa_address, TransactionType.WALLET.value)
        wallet_nonce = str(nonce_payload["nonce"])
        deadline = str(int(time.time()) + 600)
        print(f"      Attempt {attempt}: nonce={wallet_nonce}, deadline={deadline}")
        try:
            batch_resp = relayer.execute_deposit_wallet_batch(
                calls=calls,
                wallet_address=deposit_wallet,
                nonce=wallet_nonce,
                deadline=deadline,
            )
            batch_result = batch_resp.wait()
            print(f"      Batch result: {batch_result}")
            break
        except RelayerApiException as exc:
            msg = str(exc)
            transient = (
                "not registered" in msg
                or "wallet registry" in msg
                or "not yet indexed" in msg
            )
            if not transient or attempt == 6:
                raise
            wait_s = min(2 ** attempt, 30)
            print(f"      Transient registry error: {msg.splitlines()[0]}")
            print(f"      Waiting {wait_s}s for relayer indexer to catch up...")
            time.sleep(wait_s)

    print("\n" + "=" * 64)
    print("Setup complete.")
    print(f"  Deposit wallet: {deposit_wallet}")
    print(f"  EOA (signer):   {eoa_address}")
    print()
    print("Next steps:")
    print(f"  1. Transfer your pUSD to {deposit_wallet}")
    print(f"     (Withdraw from your POLY_PROXY on polymarket.com → send to the")
    print(f"     deposit wallet address shown above. Or use any wallet to call")
    print(f"     pUSD.transfer({deposit_wallet}, amount).)")
    print()
    print(f"  2. Update your .env:")
    print(f"       GENGAR_SAFE_ADDRESS={deposit_wallet}")
    print(f"       GENGAR_SIG_TYPE=3")
    print()
    print(f"  3. Run: cargo run -p pmbots-gengar --release")
    print("=" * 64)
    return 0


if __name__ == "__main__":
    sys.exit(main())
