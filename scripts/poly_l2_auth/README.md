# poly-l2-auth

Diagnostic helpers for Polymarket CLOB V2 L2 API credentials. Use these when
the Rust bot can't authenticate against `clob.polymarket.com` and you need to
isolate whether the problem is on the credential side or the HMAC-signing
side.

## Setup

```bash
cd scripts/poly_l2_auth
uv sync
```

This installs `py-clob-client` (V1, from PyPI) and `py-clob-client-v2`
(from the official Polymarket GitHub repo, pinned in `uv.lock`).

## Required env vars

Each script reads from the process environment. Source the project `.env`
or export the values before running:

| Var                | Used by             | Notes                                |
| ------------------ | ------------------- | ------------------------------------ |
| `POLY_PRIVATE_KEY` | all                 | EOA private key (hex, 0x-prefixed)   |
| `POLY_FUNDER`      | all                 | Gnosis Safe / proxy funder address   |
| `POLY_API_KEY`     | `poly-test-l2` only | L2 credential to verify              |
| `POLY_SECRET`      | `poly-test-l2` only | L2 credential to verify              |
| `POLY_PASSPHRASE`  | `poly-test-l2` only | L2 credential to verify              |

`signature_type=2` (POLY_GNOSIS_SAFE) is hard-coded — that's the only path
the Rust bot uses.

## Commands

```bash
# Derive L2 creds via the V1 client (legacy path).
uv run poly-creds-v1

# Derive L2 creds via the V2 client. Tries create_or_derive,
# create_api_key, derive_api_key, and finally lists existing keys.
uv run poly-creds-v2

# Verify a set of L2 creds by calling get_balance_allowance.
# 401 => creds are wrong. SUCCESS => creds OK and any auth failure in
# the Rust bot is on the HMAC-signing side.
uv run poly-test-l2
```

Paste the output of `poly-creds-v1` / `poly-creds-v2` into the project
`.env` as `POLY_API_KEY` / `POLY_SECRET` / `POLY_PASSPHRASE`.
