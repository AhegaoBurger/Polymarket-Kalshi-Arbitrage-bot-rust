# Polymarket V2 Internals — Hard-Won Notes

Things this repo's authors had to discover by trial because the docs gloss
over them or contradict the server. Updated 2026-05-15 from live debugging
of the gengar Poly1271 path.

## Hosts

- **Production CLOB**: `https://clob.polymarket.com` (NOT `clob-v2.polymarket.com`).
  The `-v2` subdomain was the pre-cutover staging host; the SDK examples still
  point at it. See `rs-clob-client-v2` PR #53.
- **Production relayer**: `https://relayer-v2.polymarket.com/` (the deposit-wallet
  setup flow). Different from CLOB; uses Builder API credentials.
- **Positions API** (read-only, no auth): `https://data-api.polymarket.com/positions?user=<addr>`

## Account types and their flows (Polygon mainnet)

V2 production has 4 signature types. Order placement validation differs per type:

| sig_type | Name           | Wallet                              | Order `signer` field | Sign scheme           |
| -------- | -------------- | ----------------------------------- | -------------------- | --------------------- |
| 0        | EOA            | direct                              | EOA                  | standard EIP-712      |
| 1        | POLY_PROXY     | Magic/email proxy (legacy)          | EOA                  | standard EIP-712      |
| 2        | POLY_GNOSIS_SAFE | Browser-wallet Safe (legacy)      | EOA                  | standard EIP-712      |
| 3        | POLY_1271      | Deposit wallet (V2-native, ERC-1967 proxy) | **funder (deposit wallet)** | **Solady ERC-7739 wrapped** |

**Critical: legacy sig_types 0/1/2 are being phased out for new orders.** V2
production rejects them with `"maker address not allowed, please use the
deposit wallet flow"` even from fresh wallets. The deposit wallet flow
(sig_type=3) is the only path that actually trades on production today.

## Contract addresses (Polygon mainnet, sourced from rs-clob-client-v2)

- **CTF (Conditional Tokens)**: `0x4D97DCd97eC945f40cF65F87097ACe5EA0476045`
- **pUSD (collateral)**: `0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB`
- **V2 Exchange (standard)**: `0xE111180000d2663C0091e4f400237545B87B996B`
- **V2 Exchange (neg-risk)**: `0xe2222d279d744050d28e00520010520000310F59`
- **Neg-risk adapter**: `0xd91E80cF2E7be2e162c6513ceD06f1dD0dA35296`
- **Deposit wallet factory**: `0x00000000000Fb5C9ADea0298D729A0CB3823Cc07`

## L1 auth (POST `/auth/api-key`)

- POLY_ADDRESS = signer EOA, **always**. Not the funder, even for Poly1271.
  The server recovers the signer from the ECDSA signature and requires it
  to match POLY_ADDRESS. There is **no EIP-1271 path for L1 auth.** Putting
  the funder in POLY_ADDRESS returns 400 `"Invalid L1 Request headers"`.
- ClobAuth EIP-712 domain: `{ name: "ClobAuthDomain", version: "1", chainId }`
- Nonce must be unique per (signer, request) — reusing nonce 0 from pre-V2
  returns 400 `"Could not derive api key"`. Use unix-nanoseconds.

## L2 auth (POST `/order`, `/balance-allowance`, etc.)

- HMAC-SHA256 of `timestamp + method + path + body`. The L2 secret returned
  by `/auth/api-key` is **URL_SAFE-base64-encoded**; decode to raw bytes
  before using as the HMAC key.
- POLY_ADDRESS = signer EOA (same as L1).
- `/balance-allowance` requires `signature_type` query param so the server
  knows which account variant to resolve. Without it, Safe users see $0.

## V2 Order EIP-712 schema (vs V1)

V2 dropped `taker`, `expiration`, `nonce`, `feeRateBps` from the signed
struct and added `timestamp` (ms), `metadata` (bytes32), `builder` (bytes32).
The wire body (POST /order) still includes `taker` and `expiration` but
they're not part of the signature digest.

```
Order(
  uint256 salt, address maker, address signer, uint256 tokenId,
  uint256 makerAmount, uint256 takerAmount, uint8 side,
  uint8 signatureType, uint256 timestamp, bytes32 metadata, bytes32 builder
)
```

Domain:
```
{ name: "Polymarket CTF Exchange", version: "2", chainId: 137,
  verifyingContract: <V2 Exchange std or neg-risk> }
```

## Poly1271 signing (sig_type=3)

Standard EIP-712 over Order is **NOT** what the server accepts. Instead:

1. order.maker = order.signer = funder (deposit wallet)
2. Compute `contents_hash = hashStruct(Order)` (just the struct, no domain).
3. Compute `app_domain_separator = hashStruct(EIP712Domain)` for the V2 CTF
   Exchange domain above.
4. Compute the Solady `TypedDataSign` wrapper hash:
   ```
   keccak256(typeHash_TypedDataSign || contents_hash || keccak256("DepositWallet")
             || keccak256("1") || chainId || funder || 0x0..0)
   ```
   The TypedDataSign type string is:
   ```
   TypedDataSign(Order contents,string name,string version,uint256 chainId,
                 address verifyingContract,bytes32 salt)
   Order(...same as above...)
   ```
5. Outer digest: `keccak256(0x1901 || app_domain_separator || sign_struct_hash)`
6. EOA's private key signs the outer digest.
7. **Wire signature** is NOT the 65-byte ECDSA. It's the Solady wrap:
   ```
   "0x" || inner_sig(130 hex) || app_domain_separator(64 hex)
        || contents_hash(64 hex) || ORDER_TYPE_STRING_hex || u16_be_len(4 hex)
   ```
   Total length = 264 + 2 × ORDER_TYPE_STRING.len() + 4 = 850 chars.

The deposit wallet contract validates via ERC-1271 `isValidSignature()`,
which parses the wrap and verifies the inner ECDSA against its authorized
signer set.

## Deposit wallet provisioning

A V2 deposit wallet must be **deployed + approved** before trading.
Both ops go through the relayer (not the CLOB) using Builder API creds:

1. **WALLET-CREATE** — `POST /submit` to the relayer with type `WALLET-CREATE`,
   from=EOA, to=factory. The factory uses CREATE2 to deploy a per-EOA proxy.
   Polymarket's UI auto-deploys this for new email signups; programmatic API
   users must submit it themselves. Idempotent — re-submission is harmless.

2. **WALLET batch (approvals)** — `POST /submit` with type `WALLET`, signed
   with an EIP-712 `Batch` payload over `DepositWallet` domain. The batch
   contains ERC-20 `approve(maxUint256)` calls for pUSD against each V2
   Exchange contract, plus ERC-1155 `setApprovalForAll(true)` calls for
   the CTF against each V2 Exchange contract. Without these approvals,
   the Exchange can't move tokens during fills.

3. **Fund the deposit wallet** — separate from the relayer. Any wallet that
   holds pUSD can `pUSD.transfer(<deposit_wallet>, amount)` directly. pUSD
   held by the EOA does NOT count toward the deposit wallet's CLOB balance.

4. **Sync CLOB balance cache** — `GET /balance-allowance/update?asset_type=COLLATERAL&signature_type=3`
   with L2 auth. Required after funding; until called, `GET /balance-allowance`
   returns $0 and orders fail "insufficient balance" pre-flight.

Relayer transaction states: `STATE_PENDING` → `STATE_MINED` → `STATE_CONFIRMED`.
`STATE_MINED` is sufficient for downstream calls but there's a registry-indexing
lag (~5-30s) after WALLET-CREATE before subsequent batches see the wallet.
**Retry with backoff on `"wallet registry validation failed"`.**

## Known open issues (Polymarket-side, blocking us)

- **Issue #58 on `Polymarket/rs-clob-client-v2`**: even with correct Poly1271
  signing, orders are rejected with `"the order signer address has to be the
  address of the API KEY"` because the order's `signer = funder` and the API
  key is bound to the EOA. The official Python and Rust SDKs both have this
  open. There is no documented workaround.
- **Issue #52 on `Polymarket/py-clob-client-v2`**: legacy EOA/Proxy/Safe
  flows broken on V2 production even for fresh wallets. Confirms the
  deposit wallet flow is mandatory.

## Redemption — use the CtfCollateralAdapter, not CTF directly

**Standard binary markets**: route redemption through
`CtfCollateralAdapter` at `0xAdA100Db00Ca00073811820692005400218FcE1f`.
This is the official "thin adapter" path per
`docs.polymarket.com/trading/ctf/redeem`. It has the same ABI as
`CTF.redeemPositions(address,bytes32,bytes32,uint256[])` (selector
`0x01b7037c`) and the first `address` arg is **ignored** — verified by
three live mainnet adapter txs that passed pUSD, USDC.e, and 0x0
respectively, all succeeded. The adapter internally:

1. Calls `CTF.redeemPositions(USDC.e, …)` against the deposit wallet's
   approval (`setApprovalForAll(adapter, true)` on CTF, one-time).
2. Receives USDC.e from CTF.
3. Forwards USDC.e to the pUSD contract.
4. pUSD mints 1:1 to the original sender (deposit wallet).

Result: redemption payout lands as **pUSD**, no separate wrap step needed.

**Why we initially called CTF directly and saw USDC.e**: didn't know about
the adapter. CTF.redeemPositions works but skips the wrap — payout returns
in the CTF's actual collateral (USDC.e). For markets that pre-date the pUSD
migration or are standard-binary, that's USDC.e. The asymmetry between
pUSD-debit (orders) and USDC.e-credit (CTF-direct redemption) would strand
capital over time; the adapter path closes this loop.

**For stranded USDC.e** (from prior CTF-direct redemptions): use
`scripts/setup_deposit_wallet/wrap_usdce_to_pusd.py`, which calls
`CollateralOnramp.wrap(USDC.e, deposit_wallet, amount)` to convert 1:1.

Verification: the CTF mints outcome tokens against USDC.e
(`0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174`). Computed
`CTF.getPositionId(USDC.e, CTF.getCollectionId(0, conditionId, 1))`
matches Polymarket's reported asset_id exactly; pUSD does not match.
Confirmed end-to-end: direct `CTF.redeemPositions(USDC.e, 0, conditionId,
[1,2])` mined and credited $8.00 USDC.e for 8 winning shares (tx
`0xe8a1b45e`). Pivoting to adapter would have credited pUSD instead.

**Caveat**: this is for standard binary markets (`negativeRisk: false`).
Neg-risk markets use the separately-deployed WrappedCollateral contract
owned by the NegRiskAdapter (`0xd91E80cF2E7be2e162c6513ceD06f1dD0dA35296`)
and you should call `NegRiskAdapter.redeemPositions(conditionId, amounts)`
instead of CTF directly — that adapter unwraps to the underlying.

**Local positionId derivation**: do NOT reimplement `getCollectionId` in
Python/Rust. Gnosis CTF uses a hash-to-curve on alt-bn128 (find a point on
y²=x³+3 mod P, retry x+=1 if y² isn't a quadratic residue) — a naive
`% (BN256Q-1)` is wrong. Always `eth_call` the contract's view function.

**Diagnostic**: `scripts/setup_deposit_wallet/check_balances.py` queries the
on-chain CTF balanceOf and compares to the polymarket-reported asset_id.
Useful for confirming the asset_id derivation when adding new market types.

## What works end-to-end (as of 2026-05-15)

- WALLET-CREATE + WALLET batch approvals via the relayer
- pUSD transfer into the deposit wallet
- CLOB balance cache update + balance read at sig_type=3 (returns real funding)
- POST `/auth/api-key` with EOA POLY_ADDRESS
- POST `/order` with Solady-wrapped Poly1271 signature — **server validates
  and accepts** the signature (no "Invalid signature" rejections)
- Order fills land on-chain; pUSD debit on the deposit wallet matches the
  Kelly-sized bet exactly

## What doesn't work yet

- `pending_buy` state isn't persisted across gengar restarts; an
  `UNVERIFIED_BUY` mid-window followed by Ctrl+C loses position tracking
  (the order still settles on-chain, but gengar can't claim-sell at close).
- Gengar's `resolve_window` does a claim-sell at $0.99 but doesn't yet call
  `redeemPositions(USDC.e, 0, conditionId, [1,2])` automatically on winning
  positions. Manual via `redeem_position.py` works end-to-end.
