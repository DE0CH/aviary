#!/usr/bin/env python3
"""branding.py: jarvis/deploy/branding/* -> jarvis/deploy/branding.yaml (the ConfigMap pigeon mounts over the app bundle's
/branding/) and the `services/branding-rev` annotation of pigeon's pod template (the files' hash: a change rolls
pigeon). Run it after changing a file in jarvis/deploy/branding/, and commit both.

The files go into the manifest as binaryData, not through a kustomize configMapGenerator: Flux's source controller
leaves image files (*.png, *.jpg, ...) out of what it builds from, so a generator finds no file (2026-11-13: the
tenant's reconciliation failed on it)."""
import base64, hashlib, os, re

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "deploy")
# DE0CH/claudecodeui paseo/packages/app src/yyc/brand.ts BRAND_FILES
FILES = ["logo.png", "favicon.png", "favicon-running.png", "favicon-attention.png", "apple-touch-icon.png",
         "icon-192.png", "icon-512.png", "manifest.json"]

h = hashlib.sha256()
lines = [
    "# Written by jarvis/branding.py from jarvis/deploy/branding/ (do not edit): the chat's own logo, tab and home-screen",
    "# icons and web manifest, which pigeon mounts over the app bundle's neutral /branding/ (pigeon.yaml).",
    "apiVersion: v1", "kind: ConfigMap", "metadata: { name: pigeon-branding, namespace: services }", "binaryData:",
]
for name in FILES:
    data = open(os.path.join(ROOT, "branding", name), "rb").read()
    h.update(name.encode() + b"\0" + data)
    lines.append(f"  {name}: {base64.b64encode(data).decode()}")
open(os.path.join(ROOT, "branding.yaml"), "w").write("\n".join(lines) + "\n")
rev = h.hexdigest()[:12]
p = os.path.join(ROOT, "pigeon.yaml")
s = open(p).read()
s, n = re.subn(r'services/branding-rev: "[0-9a-f]*"', f'services/branding-rev: "{rev}"', s)
assert n == 1, "pigeon.yaml has no services/branding-rev annotation"
open(p, "w").write(s)
print("pigeon-branding", rev)
