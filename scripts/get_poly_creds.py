"""Fetch Polymarket L2 API creds once — paste output into .env."""
import os

from py_clob_client.client import ClobClient
from py_clob_client.constants import POLYGON

client = ClobClient(
    host="https://clob.polymarket.com",
    chain_id=POLYGON,
    key=os.environ["POLY_PRIVATE_KEY"],
    signature_type=2,                                # POLY_GNOSIS_SAFE
    funder=os.environ["POLY_FUNDER"],
)
creds = client.create_or_derive_api_creds()
print()
print(f"POLY_API_KEY={creds.api_key}")
print(f"POLY_SECRET={creds.api_secret}")
print(f"POLY_PASSPHRASE={creds.api_passphrase}")
