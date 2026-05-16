# Polymarket V2 Deposit Wallet Setup

One-shot Python script that deploys + approves your Polymarket V2 deposit wallet
so the Rust `pmbots-gengar` bot can trade against it using `signatureType=3`
(POLY_1271 / deposit wallet flow).

This script is **only** run once per wallet. After it succeeds, gengar handles
everything from Rust against the deployed deposit wallet.

## Why this exists

Polymarket V2's order placement requires the deposit wallet flow for all new
accounts. The flow involves two separate Polymarket services:

| Service     | Purpose                              | Auth                |
| ----------- | ------------------------------------ | ------------------- |
| Relayer     | Deploy + approve wallets             | Builder API key     |
| CLOB        | Place orders, fetch balances         | CLOB API key (HMAC) |

The relayer requires builder API credentials (a separate Polymarket
registration). The official Rust SDK doesn't include a relayer client — only
TypeScript and Python do. So this Python script handles the one-time relayer
ops; gengar's Rust code handles all ongoing CLOB ops.

## Prerequisites

1. **Polymarket builder profile**: register at the Polymarket builder portal to
   get `BUILDER_API_KEY`, `BUILDER_SECRET`, `BUILDER_PASS_PHRASE`, and the
   production `RELAYER_URL`.

2. **A wallet with the EOA private key** that will own the deposit wallet. If
   you're migrating from a polymarket.com email signup, this is the same EOA
   private key you've been using with gengar (`GENGAR_PRIVATE_KEY`).

3. **Python 3.10+** and `pip`.

## Setup

```bash
cd scripts/setup_deposit_wallet
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
```

Add to your `.env` (at the workspace root):

```bash
# Already set if you've been running gengar:
GENGAR_PRIVATE_KEY=0x...

# From your Polymarket builder portal:
BUILDER_API_KEY=...
BUILDER_SECRET=...
BUILDER_PASS_PHRASE=...
RELAYER_URL=https://relayer-v2.polymarket.com/   # confirm from your builder portal
```

## Run

```bash
python setup_deposit_wallet.py
```

You should see output like:

```
Relayer URL: https://relayer-v2.polymarket.com/
Chain ID:    137 (Polygon mainnet)
EOA address: 0x4f2d...
Expected deposit wallet (CREATE2-derived): 0x...

[1/2] Submitting WALLET-CREATE...
      Deploy result: {'state': 'STATE_CONFIRMED', ...}

[2/2] Submitting WALLET batch with 6 approvals (nonce=0, deadline=...)...
      Batch result: {'state': 'STATE_CONFIRMED', ...}

================================================================
Setup complete.
  Deposit wallet: 0x...
  EOA (signer):   0x4f2d...

Next steps:
  1. Transfer your pUSD to 0x...
  2. Update your .env:
       GENGAR_SAFE_ADDRESS=0x...
       GENGAR_SIG_TYPE=3
  3. Run: cargo run -p pmbots-gengar --release
================================================================
```

## After running

1. **Transfer pUSD to the deposit wallet.** The funds currently in your
   POLY_PROXY (legacy account) need to move to the new deposit wallet:
   - Easiest: use polymarket.com's UI to withdraw pUSD from your POLY_PROXY to
     your EOA, then send pUSD from your EOA to the deposit wallet address.
   - Or: anyone can `pUSD.transfer(<deposit_wallet>, amount)` from any wallet
     that holds the pUSD token.

2. **Update `.env`** with the new deposit wallet address:
   ```bash
   GENGAR_SAFE_ADDRESS=<deposit wallet from script output>
   GENGAR_SIG_TYPE=3
   ```

3. **Run gengar**:
   ```bash
   cargo run -p pmbots-gengar --release
   ```

   The startup logs should show `[GENGAR] startup balance: $XX.XX` with the
   amount you transferred, confirming the cache sync + POLY_1271 auth flow
   are working.

## What this script approves (and why)

Six approvals submitted in a single WALLET batch:

| # | Token | Spender                  | Why                                     |
|---|-------|--------------------------|-----------------------------------------|
| 1 | pUSD  | V2 Exchange (standard)   | BUY orders on regular markets           |
| 2 | pUSD  | V2 Exchange (neg-risk)   | BUY orders on neg-risk markets          |
| 3 | pUSD  | Neg-risk Adapter         | Neg-risk settlement                     |
| 4 | CTF   | V2 Exchange (standard)   | SELL orders + position management       |
| 5 | CTF   | V2 Exchange (neg-risk)   | Neg-risk SELLs                          |
| 6 | CTF   | Neg-risk Adapter         | Neg-risk position adapter               |

Approvals are `MAX_UINT256` for pUSD (ERC-20) and `setApprovalForAll(true)` for
CTF (ERC-1155). These are *allowances* — no funds move. You can revoke any of
them later by submitting a WALLET batch with the same call but value 0 / false.

For gengar specifically (BTC Up/Down 5-min markets, NOT neg-risk), only #1 and
#4 are strictly required. The others are included so you don't have to re-run
this script if you later trade neg-risk markets.

## Troubleshooting

- **`Missing required env vars`**: set them in the workspace-root `.env`.
- **Builder credentials rejected**: confirm `RELAYER_URL` matches your builder
  portal's production environment (not staging).
- **`Deploy raised: ...already deployed`**: harmless — Polymarket auto-deploys
  the wallet for new UI signups. Approvals will still submit.
- **Balance still $0 after running gengar**: pUSD hasn't been transferred to
  the deposit wallet yet. Confirm `pUSD.balanceOf(<deposit_wallet>)` is
  non-zero on a Polygon block explorer before suspecting a bug.
