# Gengar Rust port — 2026-05-15 session handoff

Concise state of `feat/gengar-rust-port` after the day's V2 trading push.

## What got done this session

- **Gengar Rust port landed (21-task plan + post-port fixes).** Full module set: bot, strategy (Brownian + Quarter-Kelly), executor (integer-cents + ghost-fill + UNVERIFIED_BUY non-cancel), polymarket/{types, gamma, clob, ws}, market, price_feed, tracker, telegram_notifier, config, main. 36 unit tests passing.
- **Workspace migration.** `prediction-market-arbitrage` → `pmbots-arb` (verbatim move, zero `.rs` edits). New crate `pmbots-gengar` under same workspace, no shared code (Polymarket transport duplicated per "don't touch arb" rule).
- **Post-port code review fixes (opus reviewer).** V2 EIP-712 schema, camelCase wire JSON, server-derived API creds, URL_SAFE-decoded HMAC secret, `signature_type` query param on `/balance-allowance`, V2 Exchange addresses, sample-vs-population stddev for rolling vol, proper WS book ladder (handles size=0 level removal), startup balance sync from chain, retroactive `UNVERIFIED_BUY` detection, `cancel_order` on `UNVERIFIED_SELL`, halt-error pattern matching against actual Rust error strings, per-window entry lock, dry-run virtual positions for resolution logging.
- **Poly1271 (sig_type=3) wrapped signing implemented.** Solady ERC-7739 wrap: maker=signer=funder (deposit wallet), inner ECDSA by EOA, wire signature = `inner_sig || app_domain_separator || contents_hash || ORDER_TYPE_STRING || u16_be(len)`. Two new tests pin the format.
- **Python sidecar for deposit wallet provisioning.** `scripts/setup_deposit_wallet/setup_deposit_wallet.py` does WALLET-CREATE + 6-approval WALLET batch via `py-builder-relayer-client@0.0.2rc1`. Retry loop handles the relayer's wallet-registry indexing lag (~5-30s post-mine).
- **Live trade placed and filled on V2 production.** 2026-05-15 15:33 UTC: 8 UP shares of `btc-updown-5m-1778859000` at $0.87 = $6.96. Market resolved UP. Order hash `0xee23b1d2...`. **First real Poly1271 fill from gengar.**

## Current blockers (in order of operational impact)

1. ~~**`redeemPositions` returns payout=0.**~~ **RESOLVED 2026-05-15.** Two-step fix: (a) outcome tokens are minted against USDC.e (not pUSD); (b) the right call path is `CtfCollateralAdapter.redeemPositions(...)` at `0xAdA100Db00Ca00073811820692005400218FcE1f`, which wraps USDC.e → pUSD atomically. `redeem_position.py` now uses the adapter and auto-prepends `CTF.setApprovalForAll(adapter, true)` on first run; future redemptions land as pUSD directly. The initial direct-CTF redemption (tx `0xe8a1b45e1cdbc38d659e3b0154e4c776668738d31697e1c8af9461f90c2170ac`) put $8.00 USDC.e in the deposit wallet — convert via `wrap_usdce_to_pusd.py` one-shot when ready. See `docs/notes/polymarket-v2-internals.md`.
2. **Issue #58 (Polymarket-side).** Some accounts get `"the order signer address has to be the address of the API KEY"` on Poly1271 orders because the API key is bound to the EOA but order.signer = funder. Account-state-dependent: the user's fresh email-signup account did NOT hit this (orders went through), but it's a real open issue. No client-side fix.
3. **`pending_buy` state not persisted.** UNVERIFIED_BUY context lives in RAM. Ctrl+C with a pending fill mid-window loses position bookkeeping (chain settles correctly, but gengar can't claim-sell at window close). ~30 lines of disk persistence needed.
4. **`UNVERIFIED_BUY` verification window too short.** 14s (3 attempts × 3s + 5s sleep) missed a real Polygon settlement that landed in ~15-30s. Bumping `BUY_VERIFY_ATTEMPTS` from 3 to 8 would catch most late settlements while keeping the never-cancel safety property.
5. ~~**polymarket.com UI doesn't see the deposit wallet.**~~ Still true (UI shows legacy POLY_PROXY only), but redemption is now scripted via `redeem_sweep.py` so this doesn't block operations. The sidecar polls the positions API every 90s and redeems any `redeemable: true` position automatically.

## Actionable next steps (smallest first)

1. ~~Run check_balances.py / fix redeem_position.py.~~ **DONE.**
2. **Persist `pending_buy` to disk** (~30 lines in `bot.rs`): serialize to `positions.json.gengar` on every set/clear, restore on `GengarBot::new`. Resolves blocker 3.
3. **Bump `BUY_VERIFY_ATTEMPTS = 8`** in `executor.rs` (1-line config change). Tradeoff: longer per-entry latency but fewer UNVERIFIED_BUY misses. Resolves blocker 4.
4. **Optional: in-process auto-redeem in gengar.** Currently the redemption loop lives in `redeem_sweep.py` (Python sidecar, separate process). If you want it in-process, port the relayer's WALLET-batch flow to Rust (large effort — Solady ERC-7739 signing for the batch payload, distinct from the order signing already in `clob.rs`). The sidecar is fine for now.
5. **Polymarket support ticket re: Issue #58.** Server-side check that compares `order.signer` against the API-key address is the only remaining genuine Polymarket-side blocker. Account-state-dependent, no client-side fix.

## Running gengar end-to-end (two-process setup)

```
Terminal A:  cargo run -p pmbots-gengar        # trades into the deposit wallet
Terminal B:  cd scripts/setup_deposit_wallet && \
             .venv/bin/python redeem_sweep.py  # redeems resolved positions (pUSD payout via adapter)
```

One-shot: `.venv/bin/python wrap_usdce_to_pusd.py` — converts any stranded
USDC.e in the deposit wallet to pUSD via the Polymarket onramp (1:1, no fees).
Run after the initial CTF-direct redemption ($8.00 USDC.e currently in wallet)
to recycle that capital back into trading.

Both processes share state only via the chain. The sweep is safe to start/stop
independently; redeeming an already-redeemed condition is a no-op (the contract
burns zero tokens for zero payout). Set `REDEEM_SWEEP_DRY_RUN=1` to log what
would be redeemed without submitting.

On startup, gengar now logs three balance views:
- CLOB trading-available balance (single aggregate, used for Kelly sizing)
- On-chain pUSD balance (user-facing veneer)
- On-chain USDC.e balance (where redemption payouts actually land)

Set `POLYGON_RPC_URL` in `.env` to use an authenticated RPC (Alchemy/QuickNode
free tier all work) — otherwise gengar tries public endpoints in order.

## Operational gotchas to remember

- `GENGAR_SIG_TYPE=3` and `GENGAR_SAFE_ADDRESS=<deposit_wallet>` are required for the V2 trading path. Auto-detect via `funder.is_some()` picks sig_type=2 which V2 rejects for orders.
- `GENGAR_MIN_BET` interacts with integer-cents share rounding. At $5 floor + entry price >$0.50, share quantization can land below the $5 POLY_MIN_NOTIONAL. **Use ≥$7** to stay clear of the rounding floor at all valid entry prices.
- The bot autonomously trades — Ctrl+C any time you're not OK with a position firing. With `GENGAR_DAILY_LOSS_LIMIT=$10` and current ~$3 deposit wallet balance, downside is bounded.
- Deposit wallet provisioning is one-time per EOA. The 6 approvals + WALLET-CREATE never need re-running unless the EOA changes.
- Redemption payouts arrive in **USDC.e**, not pUSD. To convert to pUSD, use the Polymarket onramp contract. The bot can spend either as collateral via the existing deposit-wallet approvals.

## Reference

- Polymarket V2 internals notes: `docs/notes/polymarket-v2-internals.md`
- Original spec: `docs/superpowers/specs/2026-05-14-gengar-rust-port-design.md`
- Implementation plan: `docs/superpowers/plans/2026-05-14-gengar-rust-port.md`
- Setup script: `scripts/setup_deposit_wallet/{setup_deposit_wallet,redeem_position,check_balances}.py`
- Branch: `feat/gengar-rust-port`, 30+ commits ahead of `main`
- Live fill receipt (proof of life): polymarket.com order `0xee23b1d2621aba68615063b7d848a1d210c5237387bddf73006bceecfe3bb52c`
