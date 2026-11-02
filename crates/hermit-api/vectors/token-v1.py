#!/usr/bin/env python3
"""Computes vectors/token-v1.json from the description in docs/CONTRACTS.md alone, with the `cryptography` package
(not hermit-api), so the vector checks the Rust codec against an independent implementation:
    python3 -I crates/hermit-api/vectors/token-v1.py > crates/hermit-api/vectors/token-v1.json
"""
import base64, hashlib, hmac, struct, uuid, json, ipaddress
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
b64 = lambda b: base64.urlsafe_b64encode(b).rstrip(b'=').decode()
master = bytes([7]*32)
def hk(info):
    return HKDF(algorithm=hashes.SHA256(), length=32, salt=None, info=info).derive(master)
seed = hk(b"hermit sandbox token ed25519"); ckey = hk(b"hermit control token")
sk = Ed25519PrivateKey.from_private_bytes(seed)
pub = sk.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
sid = uuid.UUID("0190f6c2-0000-7000-8000-000000000001"); cid = uuid.UUID("0190f6c2-0000-7000-8000-000000000002")
payload = bytes([1]) + sid.bytes + cid.bytes + ipaddress.IPv4Address("192.0.2.7").packed + struct.pack(">Q", 1800000000) + b"staff@example.org"
sig = sk.sign(b"hermit-sandbox-token\0" + payload)
tok = f"hst1.1.{b64(payload)}.{b64(sig)}"
ct = "hct_" + b64(hmac.new(ckey, b"control\0" + str(sid).encode(), hashlib.sha256).digest())
print(json.dumps({"masterKey": "1:" + b64(master), "claims": {"sandboxId": str(sid), "conversationId": str(cid), "source": "192.0.2.7", "expiresAt": 1800000000, "person": "staff@example.org"},
 "payloadHex": payload.hex(), "token": tok, "publicKey": {"kid":1,"alg":"Ed25519","publicKey": b64(pub)}, "controlToken": ct, "controlTokenSha256": hashlib.sha256(ct.encode()).hexdigest()}, indent=2))
