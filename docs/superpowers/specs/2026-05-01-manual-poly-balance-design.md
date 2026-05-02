# Manual Polymarket Balance Override

## Background

The Polymarket CLOB v2 migration broke `SharedAsyncClient::fetch_poly_balance_usdc_micros`
for accounts whose API credentials were derived under v1. The 30-second background refresh
in `balance::spawn_refresh_task` therefore logs a warning every tick instead of resetting
the Polymarket side of the cache to ground truth.

Operationally this means the cache's Polymarket value starts at 0, never gets primed,
and `commit_poly` reservations have nothing to subtract from — every Polymarket-leg
opportunity is sized to zero contracts and skipped. The bot is effectively read-only
on the Polymarket side until v2 auth is fixed upstream.

## Goal

Provide a way to operate with a manually-supplied Polymarket balance, decremented
locally on each submitted order, while leaving the Kalshi balance refresh untouched.
The escape hatch should auto-disable when the env var is unset, so removing it
restores live-refresh behaviour with no code changes.

## Design

### New env var

`POLY_BALANCE_USDC` — decimal USD amount (e.g. `23.87`). Parsed once at startup,
multiplied by 1_000_000 to convert dollars to USDC micros, and written into
`BalanceCache::poly_usdc_micros` via the existing `set_poly_usdc_micros` setter.

Cents was considered (matches Kalshi's `KALSHI_API_KEY_ID`-adjacent conventions),
but USDC is a 6-decimal token and Polymarket fees are quoted in dollars, so a
decimal dollar input minimises mental conversion when topping up.

### Code changes

**`src/balance.rs`**

Add a `refresh_poly: bool` parameter to:

- `pub async fn refresh_once(...)`
- `pub fn spawn_refresh_task(...)`

When `refresh_poly == false`, `refresh_once` skips the
`poly.fetch_poly_balance_usdc_micros()` call and the corresponding `set_poly_usdc_micros`
write. The Kalshi half runs unchanged. The atomic Polymarket value in the cache is left
to be managed exclusively by `commit_poly` from execution.

**`src/main.rs`**

In the startup block around the existing prime + spawn (currently lines ~145–160):

```rust
let manual_poly_micros: Option<u64> = std::env::var("POLY_BALANCE_USDC")
    .ok()
    .and_then(|s| s.trim().parse::<f64>().ok())
    .map(|usdc| (usdc * 1_000_000.0).round() as u64);

let refresh_poly = manual_poly_micros.is_none();

if let Some(micros) = manual_poly_micros {
    balance_cache.set_poly_usdc_micros(micros);
    info!(
        "[BALANCE] Polymarket: using manual balance from POLY_BALANCE_USDC = ${:.2} \
         (auto-refresh disabled for Polymarket)",
        micros as f64 / 1_000_000.0,
    );
}

// Existing prime call: pass refresh_poly so it doesn't try to overwrite the
// manual value on startup either.
balance::refresh_once(&balance_cache, &kalshi_api, &poly_async, refresh_poly).await...

balance::spawn_refresh_task(
    balance_cache.clone(),
    kalshi_api.clone(),
    poly_async.clone(),
    refresh_poly,
);
```

Logging: a single `info!` line on startup that names the active mode is enough.
No need for periodic reminders.

## Behavioural implications

- **Local accounting only.** `commit_poly` decrements the cache on every submit.
  The cache will only ever decrease during a process lifetime.
- **External top-ups are invisible.** If the operator deposits USDC or trades on
  the Polymarket website while the bot is running, the cache will be stale until
  restart with a new `POLY_BALANCE_USDC`.
- **No hot-reload.** Re-reading the env var on each refresh tick would conflict
  with the pessimistic decrement model — every tick would reset spent dollars
  back to the env value. Restart is required to update the manual balance.
- **Kalshi unchanged.** Kalshi continues to refresh every 30s.
- **Reversible.** Unsetting `POLY_BALANCE_USDC` restores live refresh on the
  next start, no code revert needed.

## Out of scope

- A hot-reload or SIGHUP-driven balance update.
- Persisting the cache across restarts.
- A CLI subcommand or admin endpoint for manual top-ups.
- Fixing CLOB v2 auth itself (tracked separately on the
  `fix/clob-v2-existing-creds` branch).

## Testing

- Existing `BalanceCache` unit tests (atomic load/store/commit semantics) are
  unaffected — the `refresh_poly` flag is on `refresh_once` and
  `spawn_refresh_task`, not on the cache itself.
- Manual verification:
  1. Run with `POLY_BALANCE_USDC` unset → log shows live refresh attempts (and
     whatever the v2 auth path currently does).
  2. Run with `POLY_BALANCE_USDC=10.00` → log shows manual mode line, cache
     reads back $10.00, a simulated `commit_poly` reduces it, refresh ticks
     don't touch the Polymarket value.
