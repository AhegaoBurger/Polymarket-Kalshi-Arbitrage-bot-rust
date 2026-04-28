# Polymarket CLOB Fee Survey — 2026-04-21

Source: `scripts/survey_fees.sh` + targeted spot-checks against known `conditionId`s from the existing `.discovery_cache.json` (Bundesliga games) and Gamma slug searches (JD Vance 2028, Trump-before-GTA-VI, 2026 Senate midterms).

Raw JSON captured in `/tmp/fee_survey.json` (gitignored; regenerable via `bash scripts/survey_fees.sh`).

## feeSchedule presence

- [x] `feeSchedule` field present on `/markets/{condition_id}`? **NO**
- Observed in 10/10 sampled responses: `"feeSchedule": null`
- No variant field (e.g. `fee_schedule`) appeared either.

**Implication for Task 10:** `feeSchedule` path is currently a no-op. We still want the defensive code (Polymarket has signalled a move toward structured fee objects per their March 2026 rollout reporting), but it lives behind a presence check and today's markets exercise only the legacy flat-key path.

## Legacy fee keys observed

Of the eight legacy keys the existing `polymarket_clob.rs:661-670` loop tries, only one appears:

- [ ] `taker_base_fee` — **not observed** (mis-named in the existing code — the JSON uses snake_case but with the `_base_` infix)
- [ ] `takerBaseFee`
- [ ] `taker_fee_rate_bps`
- [ ] `takerFeeRateBps`
- [ ] `fee_rate_bps`
- [ ] `feeRateBps`
- [ ] `taker_fee`
- [ ] `takerFee`

**Actual observed key:** `taker_base_fee` (snake_case). This IS the first key in the existing loop (`polymarket_clob.rs:662`), so the current code already hits it. No code change needed for the field-name discovery in Task 10 — the parser already works. Task 10's value is reducing the retry loop to the two keys that actually occur and adding the `feeSchedule` check ahead of them for future-proofing.

Also observed alongside `taker_base_fee`: `maker_base_fee` (same scale, not currently consumed by the bot since it's taker-only).

## Observed per-category values

| Category  | condition_id sample                                                  | question                                                                    | `taker_base_fee` | notes                                               |
|-----------|----------------------------------------------------------------------|-----------------------------------------------------------------------------|------------------|-----------------------------------------------------|
| sports    | `0xb504b972fdfc509b95105bf7b2d919191f06fa19f487a953297b3d21ccf0cef4` | Will TSG 1899 Hoffenheim win on 2026-04-25? (Bundesliga)                    | **1000**         | tags: [Sports, bundesliga, Soccer, Games]           |
| sports    | `0x9cc039bdf25ccc4fc185739843222f35a79fe9f2dbb7947b68d266f0bbf22881` | Will 1. FSV Mainz 05 win on 2026-04-25?                                     | **1000**         | same tag set                                        |
| sports    | `0xb889fc590a72dac1d6078a73d97dda7adc10831237b8b8de73f58c6fb5c8478f` | Will FC Bayern München win on 2026-04-25?                                   | **1000**         | same tag set                                        |
| sports    | `0x4a67e1270a2ed86be8fc524b6114640e41b0c56303ecaa9584deacd62402650a` | Will the Edmonton Oilers win the 2026 NHL Stanley Cup? (NHL futures)        | **0**            | tags: [Sports, NHL, Hockey, Stanley Cup]. Futures market currently fee-free — suggests the rate is set per-market, not per-category. |
| politics  | `0x7ad403c3508f8e3912940fd1a913f227591145ca0614074208e0b962d5fcc422` | Will JD Vance win the 2028 US Presidential Election?                        | **0**            | tags: [Politics, Elections, US Election, President] |
| politics  | `0x84f8b70331323c2fba97d7ceaa9a35fb645a0770d0dbff169d07f24f376766e9` | Trump out as President before GTA VI?                                       | **0**            | tags: [Politics, Culture, All, GTA VI]              |
| politics  | `0x307a1ed89d60b61002dd5bbf00e1408c5ed2ab3fcdb056191ca7ef9bc34d38f3` | Will the Democratic Party control the Senate after the 2026 Midterms?       | **0**            | tags: [Politics, Elections, Congress, Senate]       |
| "crypto"  | `0xbb57ccf5853a85487bc3d83d04d669310d28c6c810758953b9d9b91d1aee89d2` | Will bitcoin hit $1m before GTA VI?                                         | **0**            | Misc market, tagged [Politics, Culture, GTA VI]; not a "real" crypto hourly market |
| (mixed)   | `0x9c1a953fe92c8357f1b646ba25d983aa83e90c525992db14fb726fa895cb5763` | Russia-Ukraine Ceasefire before GTA VI? (returned for every `tag_slug`)     | **0**            | demonstrates that Gamma's `tag_slug` param does not filter |
| economics | *not sampled — no active economics markets found via slug search*    |                                                                             |                  | Follow-up during PR 2 (FOMC / CPI work).            |

**Gamma caveat:** `https://gamma-api.polymarket.com/markets?tag_slug=<cat>` returns the default "recent active markets" list regardless of the `tag_slug` value — the parameter is silently ignored. The script's per-category partition is therefore not reliable on its own; always cross-check via the CLOB `tags[]` field or by targeted slug queries.

## Calibration inputs for Task B (bps_to_ppm)

**Observed Sports fee value:** `1000` (Bundesliga, three independent condition_ids, all today).
**Expected ppm for Sports (target):** `30_000` (yields 0.75% peak via `rate_ppm / 40_000`).

**Derived conversion factor:** `K = 30_000 / 1_000 = 30`. Integer. Clean.

**Decisions:**
- [x] `bps_to_ppm` implementation: **`ppm = (bps as u32) * 30`** (saturating; zero if bps ≤ 0).
- [x] `feeSchedule` vs legacy path: **legacy primary today, but Task 10 still wires a `feeSchedule` check ahead of it for forward compatibility.** Current markets all return `feeSchedule: null`, so the legacy flat-key path dominates. The existing parser (`polymarket_clob.rs:658-684`) already handles this correctly for Sports; Task 10's real change is narrowing the retry loop to `taker_base_fee` + `takerBaseFee` and adding a leading `feeSchedule` lookup.

## Unexpected: published rates vs. actual on-chain rates

The published Polymarket rates (per April 2026 reporting) are:
- Sports 0.75%, Crypto 1.80%, Economics 1.50%, Politics 1.00%, etc.

But today's actual CLOB responses show many markets with `taker_base_fee: 0` even in categories that published a non-zero rate:
- **NHL Stanley Cup futures (Sports): 0**, while Bundesliga regular-season games (Sports): 1000.
- **All three sampled Politics markets: 0**, despite the 1.00% published rate.

**Interpretation:** the CLOB `taker_base_fee` is a **per-market authoritative value**, not a category-level passthrough. Some markets within a "fee-bearing" category are currently fee-free (possibly part of a phased rollout; possibly promotional). The category table in `src/fees.rs` is therefore a **conservative ceiling** used only when the CLOB lookup fails, not a substitute for the live value.

This strengthens the case for the spec's design: CLOB is source of truth; category table is a safe-fallback ceiling for PolyCategory::Unknown or when the per-market fetch fails.

## Impact on PR 1 task text

- **Task 3:** use `K = 30` in `bps_to_ppm`. The `OBSERVED_SPORTS_BPS` constant in the unit test is `1000`.
- **Task 10:** `feeSchedule` is not present today, but the task still has value as a forward-compat layer. Keep the task but note that all existing markets will flow through the legacy path; new tests must use synthetic JSON with an injected `feeSchedule` object, not a live fetch.
- **Task 11:** when the CLOB lookup succeeds, it may return `fee_bps == 0` for some sports/politics markets. That is the correct value (not a bug), and `bps_to_ppm(0) == 0` by design. The detector will then treat the Polymarket leg as fee-free for those markets — which is accurate and opens up arb opportunities the previous hardcoded-0.75% path would have suppressed.
