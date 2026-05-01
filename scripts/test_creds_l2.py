"""Test the localStorage-extracted L2 creds via py-clob-client-v2.

If this returns a balance, the creds are valid and our Rust HMAC has a bug.
If this also returns 401, the creds themselves are wrong.
"""
import os

from py_clob_client_v2.client import ClobClient
from py_clob_client_v2.clob_types import ApiCreds, BalanceAllowanceParams

creds = ApiCreds(
    api_key=os.environ["POLY_API_KEY"],
    api_secret=os.environ["POLY_SECRET"],
    api_passphrase=os.environ["POLY_PASSPHRASE"],
)

client = ClobClient(
    host="https://clob.polymarket.com",
    chain_id=137,
    key=os.environ["POLY_PRIVATE_KEY"],
    creds=creds,
    signature_type=2,
    funder=os.environ["POLY_FUNDER"],
)

print(">>> get_balance_allowance(asset_type=COLLATERAL)")
try:
    result = client.get_balance_allowance(
        BalanceAllowanceParams(asset_type="COLLATERAL")
    )
    print("    SUCCESS:", result)
except Exception as e:
    print("    FAILED:", repr(e)[:300])
