# Gengar Rust Port and Two-Crate Workspace

## Background

The repository currently hosts a single Rust binary: a cross-platform
Kalshi↔Polymarket arbitrage bot (the "arb bot") that has been in active
development since 2025 and is in live use. The recent commit history shows
ongoing operational work — V2 WAF browser-bundle headers (`7c890dd`), centi-
contract unification (`4014e36`), heartbeat top-5 (`2440f47`).

We want to add a second, structurally different Polymarket bot to the same
repository: a faithful Rust port of
[`JLowo/gengar_polymarket_bot`](https://github.com/JLowo/gengar_polymarket_bot)
pinned at commit `9f49a07`. Gengar is a Polymarket-only oracle-lag bot that
exploits the latency between Binance BTC prices and Polymarket's deterministic
5-minute BTC Up/Down prediction markets. The Brownian-motion probability model,
Quarter-Kelly sizing, hold-to-resolution lifecycle, and ghost-fill verification
are documented in the research brief produced during brainstorming (see git
history for context).

A separate `aulekator/Polymarket-BTC-15-Minute-Trading-Bot` will later be ported
as a third workspace crate; its larger architecture (Nautilus, Grafana, Redis,
ML feedback loop) is out of scope for this spec.

## Goals

- Restructure the repository as a Cargo virtual workspace with two member
  crates: `pmbots-arb` (the existing bot, moved verbatim) and `pmbots-gengar`
  (the new port).
- Move the arb bot's source tree under `crates/pmbots-arb/` with **zero edits
  to any `.rs` file**. Only the Cargo manifest changes (package rename,
  workspace dependency wiring).
- Port gengar module-for-module from the local clone at
  `~/personal/gengar_polymarket_bot@9f49a07`. Full feature parity except where
  explicitly deferred (see Out of Scope).
- Reuse battle-tested dependency versions from arb's `Cargo.toml` via
  `[workspace.dependencies]`. Add only two gengar-specific crates: `libm` (for
  the `erf` function in the Brownian-motion CDF) and `csv` (tracker output).
- Preserve gengar's exact trading semantics: integer-cents arithmetic on the
  buy path, ghost-fill detection (success and exception paths), the
  `UNVERIFIED_BUY` non-cancel contract, the two-window `_pending_phantom`
  resolution, and the daily-loss circuit breaker with no auto-reset.

## Out of Scope (explicit deferrals)

- **Shared `polymarket-client` crate.** Both bots will independently hand-roll
  the Polymarket CLOB surface they need. Extracting a shared crate would
  require modifying `polymarket.rs` / `polymarket_clob.rs` / `balance.rs`
  inside arb, which is explicitly forbidden by the "don't touch arb code"
  constraint. The duplication is accepted as the cost of arb's stability.
  Once gengar is running and the 15-min bot is also ported, we will have
  three real consumers and can revisit extraction without arb-bot edits (by
  refactoring only the two newer crates to depend on a shared client; arb
  stays untouched until it's ever rewritten for unrelated reasons).
- **Tor / SOCKS5 proxy** (gengar's `proxy.py`). Gengar's Tor support exists
  purely to bypass Polymarket's CLOB geoblocking for users without other
  geo-mitigations. The operator of this repo already reaches Polymarket CLOB
  via arb-bot; gengar inherits the same network path. No `proxy.rs` will be
  written. Gengar's faithful behavior under `DRY_RUN=false` is preserved
  except for the Tor leg.
- **Polymarket-BTC-15-Minute-Trading-Bot port.** Will land as a third
  workspace crate (`pmbots-btc-15m` or similar) after gengar stabilizes. The
  workspace layout is designed to accommodate it but its design is a separate
  spec.
- **License compliance for redistribution.** Gengar and the 15-min bot both
  ship without a `LICENSE` file. The operator is using both for personal,
  non-distributed purposes only. If the repository is ever made public, a
  clean-room reimplementation pass against the algorithm specifications (not
  the Python expression) will be required.
- **`SAFETY_FACTOR` env var.** Documented in gengar's README, dead in code
  (`strategy.py`). Not implemented.
- **Vestigial gengar code paths.** `FORCED_EXIT_*` constants, the
  `_exit_position` function called only from claim-sell at window close, and
  the unreferenced live-exit retry logic are ported faithfully (kept but not
  wired) to preserve byte-for-byte behavior. Cleanup is a follow-up.

## Architecture

### Workspace layout

The repository root becomes a Cargo virtual workspace. No root `[package]`
section; only `[workspace]` and `[workspace.dependencies]`.

```
prediction-market-bots/                      (repo root)
├── Cargo.toml                                workspace manifest
├── Cargo.lock                                regenerated on first build
├── crates/
│   ├── pmbots-arb/                          ← `git mv` of current state
│   │   ├── Cargo.toml                       package = "pmbots-arb"
│   │   ├── src/                             ALL current src/ contents byte-for-byte
│   │   ├── tests/                           ← from root-level tests/
│   │   ├── config/                          ← from root-level config/
│   │   ├── audit/                           ← from root-level audit/
│   │   └── positions.json                   arb-only state
│   └── pmbots-gengar/                       ← brand new
│       ├── Cargo.toml                       package = "pmbots-gengar"
│       ├── src/                             (Module Map below)
│       └── tests/
├── keys/                                    workspace root (potentially shared)
├── scripts/                                 workspace root (dev utilities)
├── docs/                                    workspace root (specs, plans, notes)
├── .env                                     workspace root, namespaced by prefix
├── .gitignore                               adjusted for workspace-level target/
└── README.md                                invocation examples updated
```

### Isolation principle

`pmbots-arb` and `pmbots-gengar` share **zero source code**. They share only
workspace-pinned dependency versions for the Rust crates both happen to use.
Neither crate's `Cargo.toml` lists the other in its `[dependencies]`. Each
crate owns its own Polymarket CLOB code; updates to one (e.g. a future V2 WAF
header change) do not automatically propagate to the other.

This isolation is intentional. It accepts duplicated platform code as the
cost of arb's stability. The "rule of three" extraction (a shared
`pmbots-polymarket-client` crate) is deferred until both fresh ports
(gengar + 15-min) exist as concrete consumers, at which point extraction can
proceed without touching arb.

### Migration mechanics for arb

Allowed operations:

- `git mv src/ crates/pmbots-arb/src/`
- `git mv tests/ crates/pmbots-arb/tests/`
- `git mv config/ crates/pmbots-arb/config/`
- `git mv audit/ crates/pmbots-arb/audit/`
- `git mv positions.json crates/pmbots-arb/positions.json`
- Edit `crates/pmbots-arb/Cargo.toml`: rename `[package].name` from
  `prediction-market-arbitrage` to `pmbots-arb`. Move dependency declarations
  to `workspace = true` form where the workspace pins the same version.
- Create root `Cargo.toml` with `[workspace]` and `[workspace.dependencies]`.
- Edit `.gitignore` to relocate `target/` references to workspace root.
- Update `README.md` invocation examples (`cargo run --bin
  prediction-market-arbitrage` → `cargo run -p pmbots-arb`).

Forbidden operations (litmus test: any change inside any `.rs` file under
`crates/pmbots-arb/src/`):

- No surgery on `polymarket.rs`, `polymarket_clob.rs`, or `balance.rs`.
- No promotion of `NanoClock`, `PriceCents`, or any other type to a shared
  location.
- No splitting of the WS handler in `polymarket.rs::run_ws`.

**Acceptance test**: after migration, `cd crates/pmbots-arb && cargo build
--release` produces a binary functionally identical to today's `cargo build
--release` at the repo root.

### Gengar module map

```
crates/pmbots-gengar/src/
├── main.rs                  ~30 LoC   — entry point: load config, init bot, run
├── lib.rs                   ~20 LoC   — pub module declarations
├── bot.rs                  ~800 LoC   — main loop, position lifecycle, daily-loss CB
├── strategy.rs             ~200 LoC   — Brownian + Quarter-Kelly + entry gates
├── executor.rs             ~400 LoC   — order placement, ghost-fill verification
├── market.rs               ~150 LoC   — Gamma event lookup, window detection
├── price_feed.rs           ~200 LoC   — Binance BTCUSDT @trade WS + REST fallback
├── tracker.rs              ~400 LoC   — CSV writers (signals/trades/executions/sessions)
├── telegram_notifier.rs    ~100 LoC   — fire-and-forget Telegram POSTs
├── config.rs                ~80 LoC   — GENGAR_* env loading
└── polymarket/
    ├── mod.rs               ~10 LoC
    ├── clob.rs             ~450 LoC   — EIP-712 signing, /ok, /balance, /price, /order
    ├── gamma.rs             ~80 LoC   — Gamma REST event lookup
    ├── ws.rs               ~180 LoC   — current-token orderbook subscription
    └── types.rs             ~80 LoC   — Polymarket platform types (PriceCents, etc.)
```

Estimated total: ~3180 LoC. Comparable to the Python source (3186 LoC across 8
modules), with the increase concentrated in `polymarket/` (replacing
`py-clob-client` which Python uses as a black-box dependency).

### Module dependency graph

```
bot.rs  ──depends on──►  strategy, executor, market, price_feed,
                          tracker, telegram_notifier, config,
                          polymarket::ws        (owns the WS subscription)
executor.rs  ──────────►  polymarket::clob, polymarket::gamma
market.rs  ────────────►  polymarket::gamma
strategy.rs  ──────────────  (pure math, no deps)
price_feed.rs  ────────────  (binance only, no Polymarket deps)
tracker.rs  ───────────────  (filesystem only)
telegram_notifier.rs  ─────  (Telegram HTTP only)
polymarket/clob.rs  ──►  polymarket::types
polymarket/gamma.rs  ──────  (raw reqwest + parse)
polymarket/ws.rs  ────►  polymarket::types
polymarket/types.rs  ──────  (intra-crate platform types)
```

Acyclic. Implementation order: `polymarket/` first, then independents
(`strategy`, `market`, `price_feed`, `tracker`, `telegram_notifier`), then
`executor`, finally `bot`.

**WS lifecycle**: `bot.rs` initiates the Polymarket WS subscription at each
new window boundary against the current window's `token_id_up` and
`token_id_down`. The WS feed updates an in-memory `Arc<RwLock<TokenPriceCache>>`
that `strategy.rs` reads on each entry-evaluation tick. The subscription is
torn down and re-established at every window transition (every 5 minutes for
the default `GENGAR_MARKET_PERIOD=5`). Gengar's Python does not have this WS
loop; it polls `calculate_market_price` per entry attempt. The Rust port
adds the WS feed specifically to reduce entry-decision latency, which is the
core reason for the port.

## Dependency Choices

### Workspace-pinned (reused from arb)

```toml
tokio              = { version = "1.0",  features = ["full"] }
tokio-tungstenite  = { version = "0.21", features = ["native-tls"] }
reqwest            = { version = "0.11", features = ["json", "blocking"] }
serde              = { version = "1.0",  features = ["derive", "rc"] }
serde_json         = "1.0"
anyhow             = "1.0"
chrono             = { version = "0.4",  features = ["serde"] }
tracing            = "0.1"
tracing-subscriber = { version = "0.3",  features = ["env-filter"] }
dotenvy            = "0.15"
ethers             = { version = "2.0",  features = ["legacy"] }
futures-util       = "0.3"
async-trait        = "0.1"
```

### Gengar-specific

```toml
libm = "0.2"      # erf() for Brownian-motion CDF
csv  = "1.3"      # tracker output: signals.csv, trades.csv, executions.csv, sessions.csv
```

`libm` is chosen over `statrs` for minimal dependency footprint. `libm` is a
pure-Rust port of musl's libm: ~30 functions, no transitive dependencies, no
overhead beyond the single `erf` call gengar's Brownian model needs. If the
strategy is ever enriched with more distribution math, `statrs` can be added
later.

## Config Schema

### Loading

Single `.env` at workspace root, loaded by both crates via
`dotenvy::dotenv()`. Vars namespaced by prefix:

- `POLY_*`, `KALSHI_*`, `CB_*`, etc. — arb's existing vars, unchanged
- `GENGAR_*` — new, gengar-specific

### Gengar `.env` variables (strict, no fallback)

| Var | Default | Notes |
|---|---|---|
| **Wallet** (required for live, no fallback to POLY_*) | | |
| `GENGAR_PRIVATE_KEY` | — | EOA private key. Required when `GENGAR_DRY_RUN=false`. |
| `GENGAR_SAFE_ADDRESS` | — | Polymarket Safe proxy. If set, `sig_type=2`; else `sig_type=0`. |
| **Mode** | | |
| `GENGAR_DRY_RUN` | `true` | Default ON. Explicitly `false` to trade live. |
| **Strategy** | | |
| `GENGAR_MIN_EDGE` | `0.05` | Min `true_prob - market_price` to enter. |
| `GENGAR_MIN_PROB` | `0.80` | Min model probability to enter. |
| `GENGAR_MIN_BTC_DELTA` | `0.06` | Min `|btc_delta_pct|`. Guard against zero-cross. |
| `GENGAR_ENTRY_WINDOW_START` | `240` | Seconds-to-resolution upper bound for entry. |
| `GENGAR_ENTRY_WINDOW_END` | `10` | Seconds-to-resolution lower bound for entry. |
| `GENGAR_KELLY_FRACTION` | `0.25` | Quarter-Kelly. |
| `GENGAR_MIN_BET` | `5.0` | USD floor per trade (Polymarket min notional). |
| `GENGAR_MAX_BET` | `25.0` | USD ceiling per trade. |
| `GENGAR_BANKROLL` | `100.0` | Seed; overwritten by real USDC balance when live. |
| **Volatility** | | |
| `GENGAR_ROLLING_VOL_WINDOWS` | `12` | Rolling stdev sample size. |
| `GENGAR_VOL_FLOOR` | `0.06` | Lower clamp. |
| `GENGAR_VOL_CAP` | `0.30` | Upper clamp. |
| **Risk** | | |
| `GENGAR_DAILY_LOSS_LIMIT` | `30.0` | Session-PnL halt. No auto-reset; halt persists until process restart. |
| **Notification** | | |
| `GENGAR_TELEGRAM_BOT_TOKEN` | `""` | Telegram is no-op if either var unset. |
| `GENGAR_TELEGRAM_CHAT_ID` | `""` | |
| **System** | | |
| `GENGAR_MARKET_PERIOD` | `5` | Minutes. Only 5 or 15 supported. |
| `GENGAR_LOG_DIR` | `logs/gengar` | CSV output dir; namespaced to avoid colliding with arb. |
| `GENGAR_LOG_EXECUTIONS` | `false` | Opt-in `executions.csv`. |

Wallet fallback is intentionally **strict**: gengar reads only `GENGAR_*` and
will not fall back to `POLY_*`. For shared-wallet operation, the operator
duplicates the values in `.env` explicitly. This prevents the failure mode
where gengar accidentally trades from the arb wallet during dry-run testing.

## Implementation Gotchas

These are translation hazards documented from the gengar research brief.
They must be ported faithfully; any deviation changes trading semantics.

1. **Integer-cents arithmetic on the buy path** (`executor.py:56-74`).
   Polymarket CLOB rejects orders with float-precision artifacts like
   `21.000000000004`. Port verbatim:
   ```
   price_cents = round(price * 100)
   max_shares  = (max_usd * 100) // price_cents
   clean_usd   = (max_shares * price_cents) / 100
   ```
   Never use direct `usd / price` division.

2. **Never cancel an `UNVERIFIED_BUY`** (`executor.py:294-300`). If neither
   the balance delta nor `get_order` confirm a fill within ~14s, return
   `Failed` with `error=UNVERIFIED_BUY`. Do not cancel. Polygon settlement
   can take 5–15s and a cancel-then-fill race causes double-fills. The bot's
   `_pending_phantom` mechanism reconciles at the next window boundary.

3. **Ghost-fill exception path** (`executor.py:238-254`, `388-397`). On any
   HTTP exception during buy or sell, sleep 3s, recheck balance, and if the
   delta > $1.00 (buy) or > $0.10 (sell), declare the order filled
   regardless of the exception. Return `order_id="ghost-buy"` or
   `"ghost-sell"` so tracker can distinguish.

4. **`_pending_phantom` two-window resolution** (`bot.py:436-485`). When a
   claim-sell at window close returns API success but balance has not
   moved, defer resolution to the *next* window boundary. This handles
   delayed Polygon settlement of legitimate fills.

5. **Daily-loss CB has no auto-reset** (`bot.py:682-694, 187`).
   `session_start_balance` is captured once at process startup. After a
   trip, halt persists until restart. Faithful behavior is intentional —
   do not add a midnight reset.

6. **`SAFETY_FACTOR` is dead code.** Documented in gengar's README, never
   read in `strategy.py`. Do not implement.

7. **Window-boundary balance sync** (`bot.py:549-558`). Every 5-minute
   boundary, the bot overwrites internal `stats.bankroll` with the on-chain
   USDC balance if drift exceeds $0.50. Source of truth is always the chain.

8. **Health check is a literal HTTP GET** (`client.get_ok()` in
   py-clob-client). In Rust: `GET https://clob.polymarket.com/ok`, expect
   200. Called before every entry attempt (`bot.py:669`) and at each new
   window if `_clob_halted` for auto-recovery (`bot.py:594-599`).

9. **3-strike CLOB halt** (`bot.py:816-823`). On error strings matching
   `"request exception" | "service not ready" | "status_code=none"`, increment
   counter. Third consecutive sets `_clob_halted=true`. Recovery at next
   window boundary when `get_ok()` succeeds.

10. **Browser-bundle V2 WAF headers**. Gengar's Python uses py-clob-client
    which uses httpx under the hood; the V2 WAF cutover (April 2026) affected
    arb-bot (commit `7c890dd`) but gengar's Python via py-clob-client may have
    a different mitigation path. The Rust port mirrors arb's UA + `sec-ch-ua-*`
    + `Accept-Language` headers in `polymarket/clob.rs`. Verify against live
    CLOB behavior during implementation; this is the highest-risk untested
    assumption.

## Logging

Both crates use `tracing` + `tracing-subscriber` with `EnvFilter`, matching
arb's existing setup. Gengar's log lines use the `[GENGAR]` prefix to keep
output distinguishable when both bots are running in adjacent terminals.
Standard `RUST_LOG=info,pmbots_gengar=debug` style env override.

## Reference

- Local gengar clone: `~/personal/gengar_polymarket_bot` pinned at commit
  `9f49a07`.
- Local 15-min bot clone: `~/personal/Polymarket-BTC-15-Minute-Trading-Bot`
  (out of scope for this spec).
- Author-provided context: gengar's `CLAUDE.md` and `SETUP.md` are useful
  during implementation.
- Research brief covering gengar's exact strategy math, executor flow, bot
  state machine, and `.env` schema was produced during brainstorming.
