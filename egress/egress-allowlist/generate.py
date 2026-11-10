#!/usr/bin/env python3
"""Build the sandbox egress lists (docs/CHAT.md, "The sandbox internet rules").

A sandbox may reach every host except those on the deny list (Deyao, 2026-11-10: fewer false refusals). Writes
allow.txt and deny.txt next to this script from:
  deny: sources/deny.txt (curated: anonymous upload, file sharing, object storage, request catchers, tunnels,
    anyone-runs-a-server hosting, forms, webmail), every domain in the UT1 web classification's CATEGORIES
    (Universite Toulouse Capitole, CC BY-SA 4.0), and every Public Suffix List private-section suffix (MPL 2.0:
    user-content platforms such as .workers.dev, .github.io, .herokuapp.com where anyone can run or publish something)
  allow: sources/dev.txt, sources/yyc.txt (curated; an entry more specific than a deny entry lets that host through,
    e.g. the exact raw.githubusercontent.com under the denied .githubusercontent.com)

Syntax of both outputs: `host` = exactly that name; `.domain` = the domain and every name under it. The egress proxy
refuses a host when its most specific deny entry is at least as specific as its most specific allow entry.

Python 3.10+, standard library only. Usage: generate.py [--cache DIR]
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import io
import os
import re
import sys
import tarfile
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
UA = "jarvis3-egress-allowlist/1 (+https://github.com/DE0CH/jarvis3)"

PSL_URL = "https://publicsuffix.org/list/public_suffix_list.dat"
UT1_URL = "https://dsi.ut-capitole.fr/blacklists/download/{}.tar.gz"

# UT1 categories on the deny list: attack and abuse tools, file sharing, anonymisers, redirectors/shorteners, dynamic DNS, webmail and
# chat (both take messages), remote control, free web hosting. Not malware, phishing or cryptojacking: egress's threat-intel
# DNS refuses those on every connection, from fresher lists (240,000 entries here). Not adult: no exfiltration risk.
CATEGORIES = [
    "ddos", "dialer", "stalkerware", "hacking", "warez",
    "filehosting", "webhosting",
    "redirector", "strict_redirector", "strong_redirector", "shortener",
    "proxy", "vpn", "residential-proxies", "doh", "dynamic-dns",
    "webmail", "chat", "remote-control",
]


def fetch(url: str, cache: str | None) -> bytes:
    if cache:
        p = os.path.join(cache, re.sub(r"[^A-Za-z0-9._-]", "_", url))
        if os.path.exists(p):
            return open(p, "rb").read()
    with urllib.request.urlopen(urllib.request.Request(url, headers={"user-agent": UA}), timeout=300) as r:
        data = r.read()
    if cache:
        os.makedirs(cache, exist_ok=True)
        open(p, "wb").write(data)
    return data


class Psl:
    """Registrable domains by the ICANN section; the private section is kept apart (platform suffixes)."""

    def __init__(self, text: str):
        self.rules: set[str] = set()
        self.wild: set[str] = set()
        self.exc: set[str] = set()
        self.private: list[str] = []
        private = False
        for line in text.splitlines():
            line = line.strip()
            if "===BEGIN PRIVATE DOMAINS===" in line:
                private = True
            if not line or line.startswith("//"):
                continue
            rule = line.split()[0].encode("idna").decode("ascii") if not line.isascii() else line.split()[0].lower()
            if private:
                self.private.append(rule)
                continue
            if rule.startswith("!"):
                self.exc.add(rule[1:])
            elif rule.startswith("*."):
                self.wild.add(rule[2:])
            else:
                self.rules.add(rule)

    def suffix_len(self, labels: list[str]) -> int:
        """Number of labels of the public suffix of `labels`."""
        for i in range(len(labels)):
            name = ".".join(labels[i:])
            if name in self.exc:
                return len(labels) - i - 1
            if name in self.rules:
                return len(labels) - i
            if i + 1 < len(labels) and ".".join(labels[i + 1:]) in self.wild:
                return len(labels) - i
        return 1  # unknown TLD: the default rule "*"

    def registrable(self, host: str) -> str | None:
        host = host.strip().lower().rstrip(".")
        if not host or not re.fullmatch(r"[a-z0-9_.-]+", host) or re.fullmatch(r"[0-9.]+", host):
            return None
        labels = [x for x in host.split(".") if x]
        n = self.suffix_len(labels)
        if len(labels) <= n:
            return None
        return ".".join(labels[-(n + 1):])


def read_list(name: str) -> list[str]:
    out = []
    for line in open(os.path.join(HERE, "sources", name), encoding="utf-8"):
        e = line.split("#", 1)[0].strip().lower()
        if e:
            out.append(e)
    return out


def covered(host: str, entries: set[str]) -> bool:
    """Is `host` matched by an entry (exact, or a `.suffix` entry)?"""
    if host in entries:
        return True
    labels = host.split(".")
    return any("." + ".".join(labels[i:]) in entries for i in range(len(labels)))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cache", help="keep downloads here (development)")
    a = ap.parse_args()

    psl = Psl(fetch(PSL_URL, a.cache).decode())
    curated_allow = set(read_list("dev.txt")) | set(read_list("yyc.txt"))
    curated_deny = set(read_list("deny.txt"))

    ut1: dict[str, str] = {}
    for cat in CATEGORIES:
        try:
            blob = fetch(UT1_URL.format(cat), a.cache)
        except Exception as e:  # a category UT1 no longer has
            print(f"UT1 {cat}: {e}", file=sys.stderr)
            continue
        with tarfile.open(fileobj=io.BytesIO(blob), mode="r:gz") as t:
            for m in t.getmembers():
                if not m.isfile() or not m.name.endswith("/domains"):
                    continue
                for raw in t.extractfile(m).read().decode(errors="replace").splitlines():
                    h = raw.strip().lower().rstrip(".")
                    if not h or h.startswith("#") or not re.fullmatch(r"[a-z0-9_.-]+", h) or re.fullmatch(r"[0-9.]+", h):
                        continue
                    if not psl.registrable(h) or covered(h, curated_allow):
                        continue
                    ut1.setdefault("." + h, cat)

    platform: set[str] = set()
    for rule in psl.private:
        if rule.startswith("!"):
            continue
        suffix = rule[2:] if rule.startswith("*.") else rule
        if suffix not in curated_allow and "." + suffix not in curated_allow:
            platform.add("." + suffix)

    deny_all = (curated_deny | set(ut1) | platform) - curated_allow
    suffixes = {x for x in deny_all if x.startswith(".")}
    # an entry inside a broader deny suffix adds nothing
    deny = sorted(e for e in deny_all if "." not in e.lstrip(".") or not covered(e.lstrip(".").split(".", 1)[1], suffixes))
    allow = sorted(curated_allow)

    today = dt.date.today().isoformat()
    sources = (
        "# sources: deny = curated deny.txt + UT1 categories (Universite Toulouse Capitole, CC BY-SA 4.0) + Public Suffix "
        "List private-section platforms (MPL 2.0); allow = curated dev.txt + yyc.txt (exceptions to the deny list)\n"
    )

    def write(name: str, entries: list[str], extra: str) -> None:
        body = "".join(e + "\n" for e in entries)
        digest = hashlib.sha256(body.encode()).hexdigest()[:16]
        head = (
            f"# Sandbox egress {name.split('.')[0]}list, generated by generate.py (docs/CHAT.md, \"The sandbox internet rules\"). Do not edit:\n"
            f"# change sources/ and run generate.py. `host` = that name only; `.domain` = the domain and everything under it.\n"
            f"# version: {today} {digest}\n# entries: {len(entries)}\n{sources}{extra}"
        )
        open(os.path.join(HERE, name), "w", encoding="utf-8").write(head + body)

    by_cat: dict[str, int] = {}
    for c in ut1.values():
        by_cat[c] = by_cat.get(c, 0) + 1
    write("allow.txt", allow, "")
    write("deny.txt", deny, f"# curated {len(curated_deny)}, UT1 {len(ut1)}, platforms {len(platform)}\n")
    print(f"allow {len(allow)}; deny {len(deny)} (curated {len(curated_deny)}, platforms {len(platform)}, UT1 {by_cat})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
