"""Fetch Polymarket V2 L2 API creds — paste output into .env.

Uses py_clob_client_v2 explicitly (not the V1 client). Also tries get_api_keys
(L1-headers GET on /auth/api-keys, plural) which py_clob_client_v2 has but V1
does not — this returns existing keys without creating new ones.
"""

import os
import sys
import traceback

from py_clob_client_v2.client import ClobClient


def _print_creds(creds) -> None:
    print()
    print(f"POLY_API_KEY={creds.api_key}")
    print(f"POLY_SECRET={creds.api_secret}")
    print(f"POLY_PASSPHRASE={creds.api_passphrase}")


def main() -> None:
    key = os.environ["POLY_PRIVATE_KEY"]
    funder = os.environ["POLY_FUNDER"]

    client = ClobClient(
        host="https://clob.polymarket.com",
        chain_id=137,
        key=key,
        signature_type=2,
        funder=funder,
    )

    print(">>> trying create_or_derive_api_key()")
    try:
        _print_creds(client.create_or_derive_api_key())
        sys.exit(0)
    except Exception as e:
        print("    failed:", repr(e)[:200])

    print(">>> trying create_api_key()")
    try:
        _print_creds(client.create_api_key())
        sys.exit(0)
    except Exception as e:
        print("    failed:", repr(e)[:200])

    print(">>> trying derive_api_key()")
    try:
        _print_creds(client.derive_api_key())
        sys.exit(0)
    except Exception as e:
        print("    failed:", repr(e)[:200])

    print(">>> trying get_api_keys() (will only list api_keys, not secrets)")
    try:
        keys = client.get_api_keys()
        print(f"existing api_keys: {keys}")
    except Exception as e:
        print("    failed:", repr(e)[:200])
        traceback.print_exc()

    print()
    print("All attempts exhausted.")


if __name__ == "__main__":
    main()
