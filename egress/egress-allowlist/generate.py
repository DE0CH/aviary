#!/usr/bin/env python3
"""Build the sandbox egress allowlist (docs/CHAT.md, "The sandbox allowlist").

Writes allow.txt and deny.txt next to this script from:
  sources/dev.txt, sources/yyc.txt   curated allow sets (always in)
  sources/deny.txt                   curated deny set
  a ranked set: the top N registrable domains by the Tranco method (Dowdall rule over several popularity lists,
    Le Pochat et al., NDSS 2019) over the three providers whose terms allow any use: the Chrome UX Report (CC BY-SA
    4.0), the Majestic Million (CC BY 3.0) and the Cisco Umbrella top 1M (free of charge). The published Tranco list
    itself also folds in Cloudflare Radar (CC BY-NC 4.0) and Farsight, so it is not used directly.
  minus risky categories from the UT1 web classification (Universite Toulouse Capitole, CC BY-SA 4.0): a domain in
    one of CATEGORIES is dropped; a listed subdomain of a kept domain goes on the deny list.
  minus user-content platforms: every Public Suffix List private-section suffix (MPL 2.0) under a kept domain goes on
    the deny list (e.g. .workers.dev, .github.io, .herokuapp.com: anyone can run or publish something there).

Syntax of both outputs: `host` = exactly that name; `.domain` = the domain and every name under it. The egress proxy
lets a host through when its most specific allow entry is more specific than its most specific deny entry.

Python 3.10+, standard library only. Usage: generate.py [--top 50000] [--cache DIR]
"""
from __future__ import annotations

import argparse
import csv
import datetime as dt
import gzip
import hashlib
import io
import os
import re
import sys
import tarfile
import urllib.request
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
UA = "jarvis3-egress-allowlist/1 (+https://github.com/DE0CH/jarvis3)"

PSL_URL = "https://publicsuffix.org/list/public_suffix_list.dat"
CRUX_URL = "https://raw.githubusercontent.com/zakird/crux-top-lists/main/data/global/current.csv.gz"
MAJESTIC_URL = "https://downloads.majestic.com/majestic_million.csv"
UMBRELLA_URL = "https://s3-us-west-1.amazonaws.com/umbrella-static/top-1m.csv.zip"
UT1_URL = "https://dsi.ut-capitole.fr/blacklists/download/{}.tar.gz"

# UT1 categories that are never on the list: threats, adult, file sharing, anonymisers, redirectors/shorteners,
# dynamic DNS, webmail and chat (both take messages), remote control, free web hosting. Adult only drops ranked domains
# (its millions of listed subdomains on blog platforms carry no exfiltration risk and would make the deny list huge).
CATEGORIES = [
    "malware", "phishing", "cryptojacking", "ddos", "dialer", "stalkerware", "hacking", "warez",
    "adult", "mixed_adult", "porn",
    "filehosting", "webhosting",
    "redirector", "strict_redirector", "strong_redirector", "shortener",
    "proxy", "vpn", "residential-proxies", "doh", "dynamic-dns",
    "webmail", "chat", "remote-control",
]
DROP_ONLY = {"adult", "mixed_adult", "porn"}


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


def ranked(urlrows, psl: Psl, limit: int = 1_000_000) -> dict[str, int]:
    """registrable domain -> best rank (first appearance)."""
    out: dict[str, int] = {}
    for rank, host in urlrows:
        d = psl.registrable(host)
        if d and d not in out:
            out[d] = rank
        if len(out) >= limit:
            break
    return out


def crux_rows(data: bytes):
    rows = csv.reader(io.StringIO(gzip.decompress(data).decode()))
    next(rows)
    for origin, rank in rows:
        yield int(rank), re.sub(r"^[a-z]+://", "", origin).split(":")[0].split("/")[0]


def majestic_rows(data: bytes):
    rows = csv.reader(io.StringIO(data.decode(errors="replace")))
    head = next(rows)
    gi, di = head.index("GlobalRank"), head.index("Domain")
    for r in rows:
        yield int(r[gi]), r[di]


def umbrella_rows(data: bytes):
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        text = z.read(z.namelist()[0]).decode(errors="replace")
    for line in text.splitlines():
        rank, _, host = line.partition(",")
        if host:
            yield int(rank), host


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
    ap.add_argument("--top", type=int, default=50_000)
    ap.add_argument("--cache", help="keep downloads here (development)")
    a = ap.parse_args()

    psl = Psl(fetch(PSL_URL, a.cache).decode())
    lists = {
        "crux": ranked(crux_rows(fetch(CRUX_URL, a.cache)), psl),
        "majestic": ranked(majestic_rows(fetch(MAJESTIC_URL, a.cache)), psl),
        "umbrella": ranked(umbrella_rows(fetch(UMBRELLA_URL, a.cache)), psl),
    }
    score: dict[str, float] = {}
    for ranks in lists.values():
        for d, r in ranks.items():
            score[d] = score.get(d, 0.0) + 1.0 / r
    top = [d for d, _ in sorted(score.items(), key=lambda kv: (-kv[1], kv[0]))[: a.top]]
    top_set = set(top)

    curated_allow = set(read_list("dev.txt")) | set(read_list("yyc.txt"))
    curated_deny = set(read_list("deny.txt"))

    # UT1: a listed ranked domain is dropped; a listed subdomain of a ranked domain is denied (unless curated)
    dropped: dict[str, str] = {}
    sub_deny: set[str] = set()
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
                    if not h or h.startswith("#"):
                        continue
                    if h in top_set:
                        dropped.setdefault(h, cat)
                        continue
                    reg = psl.registrable(h)
                    if cat not in DROP_ONLY and reg in top_set and h != reg and not covered(h, curated_allow):
                        sub_deny.add("." + h)

    kept = [d for d in top if d not in dropped]
    kept_set = set(kept)

    # PSL private section: platform suffixes under a kept or curated domain
    platform_deny: set[str] = set()
    for rule in psl.private:
        if rule.startswith("!"):
            continue
        suffix = rule[2:] if rule.startswith("*.") else rule
        reg = psl.registrable(suffix)
        if not reg:
            continue
        if (reg in kept_set or covered(suffix, curated_allow)) and suffix not in curated_allow and "." + suffix not in curated_allow:
            platform_deny.add("." + suffix)

    allow = sorted(curated_allow | {"." + d for d in kept})
    deny_all = (curated_deny | sub_deny | platform_deny) - curated_allow
    # an entry inside a broader deny suffix adds nothing
    deny = sorted(e for e in deny_all if not covered(e.lstrip(".").split(".", 1)[-1], {x for x in deny_all if x.startswith(".")}) or "." not in e.lstrip("."))

    today = dt.date.today().isoformat()
    sources = (
        f"# sources: curated dev.txt + yyc.txt; top {a.top} registrable domains by the Tranco method (Dowdall) over CrUX "
        "(CC BY-SA 4.0), Majestic Million (CC BY 3.0), Cisco Umbrella top 1M (free of charge); minus UT1 categories "
        "(Universite Toulouse Capitole, CC BY-SA 4.0); deny = curated deny.txt + UT1 subdomains + Public Suffix List "
        "private-section platforms (MPL 2.0)\n"
    )

    def write(name: str, entries: list[str], extra: str) -> None:
        body = "".join(e + "\n" for e in entries)
        digest = hashlib.sha256(body.encode()).hexdigest()[:16]
        head = (
            f"# Sandbox egress {name.split('.')[0]}list, generated by generate.py (docs/CHAT.md, \"The sandbox allowlist\"). Do not edit:\n"
            f"# change sources/ and run generate.py. `host` = that name only; `.domain` = the domain and everything under it.\n"
            f"# version: {today} {digest}\n# entries: {len(entries)}\n{sources}{extra}"
        )
        open(os.path.join(HERE, name), "w", encoding="utf-8").write(head + body)

    write("allow.txt", allow, f"# ranked: {len(top)} candidates, {len(dropped)} dropped by category, {len(kept)} kept\n")
    write("deny.txt", deny, f"# curated {len(curated_deny)}, UT1 subdomains {len(sub_deny)}, platforms {len(platform_deny)}\n")
    by_cat: dict[str, int] = {}
    for c in dropped.values():
        by_cat[c] = by_cat.get(c, 0) + 1
    print(f"allow {len(allow)} (ranked kept {len(kept)}, dropped {len(dropped)}: {by_cat}); deny {len(deny)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
