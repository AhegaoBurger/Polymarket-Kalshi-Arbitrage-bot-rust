"""Fetch Polymarket V2 L2 API creds — paste output into .env.

Uses py_clob_client_v2 explicitly (not the V1 client). Also tries get_api_keys
(L1-headers GET on /auth/api-keys, plural) which py_clob_client_v2 has but V1
does not — this returns existing keys without creating new ones.
"""

import os
import traceback

from py_clob_client_v2.client import ClobClient

KEY = os.environ["POLY_PRIVATE_KEY"]
FUNDER = os.environ["POLY_FUNDER"]

client = ClobClient(
    host="https://clob.polymarket.com",
    chain_id=137,
    key=KEY,
    signature_type=2,
    funder=FUNDER,
)

# 1) try create_or_derive
print(">>> trying create_or_derive_api_key()")
try:
    creds = client.create_or_derive_api_key()
    print()
    print(f"POLY_API_KEY={creds.api_key}")
    print(f"POLY_SECRET={creds.api_secret}")
    print(f"POLY_PASSPHRASE={creds.api_passphrase}")
    raise SystemExit(0)
except Exception as e:
    print("    failed:", repr(e)[:200])

# 2) try create_api_key directly
print(">>> trying create_api_key()")
try:
    creds = client.create_api_key()
    print()
    print(f"POLY_API_KEY={creds.api_key}")
    print(f"POLY_SECRET={creds.api_secret}")
    print(f"POLY_PASSPHRASE={creds.api_passphrase}")
    raise SystemExit(0)
except SystemExit:
    raise
except Exception as e:
    print("    failed:", repr(e)[:200])

# 3) try derive_api_key
print(">>> trying derive_api_key()")
try:
    creds = client.derive_api_key()
    print()
    print(f"POLY_API_KEY={creds.api_key}")
    print(f"POLY_SECRET={creds.api_secret}")
    print(f"POLY_PASSPHRASE={creds.api_passphrase}")
    raise SystemExit(0)
except SystemExit:
    raise
except Exception as e:
    print("    failed:", repr(e)[:200])

# 4) try get_api_keys (lists existing keys but secrets aren't returned)
print(">>> trying get_api_keys() (will only list api_keys, not secrets)")
try:
    keys = client.get_api_keys()
    print(f"existing api_keys: {keys}")
except Exception as e:
    print("    failed:", repr(e)[:200])
    traceback.print_exc()

print()
print("All attempts exhausted.")
