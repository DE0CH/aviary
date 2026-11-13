#!/usr/bin/env python3
"""The editing library of the image: music, sound effects and animated emoji, fetched from where they are published
by a manifest that pins each file's sha256 (a file that changed at its source fails the build), and the index files
the `edit` command reads.

    fetch-assets.py MANIFEST.json /opt/assets

Music and sound effects are Mixkit's (its free licence: use in videos, commercial ones included, with no credit; the
files themselves are not to be redistributed, which is why they are fetched from Mixkit and not kept in this
repository or in a release). The animated emoji are Google's Noto set (CC BY 4.0)."""
import concurrent.futures
import hashlib
import json
import os
import sys
import time
import urllib.request

UA = {"User-Agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/126 Safari/537.36"}


def fetch(entry, root):
    path = os.path.join(root, entry["file"])
    os.makedirs(os.path.dirname(path), exist_ok=True)
    last = None
    for attempt in range(5):
        try:
            data = urllib.request.urlopen(urllib.request.Request(entry["url"], headers=UA), timeout=180).read()
            got = hashlib.sha256(data).hexdigest()
            if got != entry["sha256"]:
                raise SystemExit(f"{entry['url']}: sha256 {got}, the manifest says {entry['sha256']}")
            with open(path, "wb") as f:
                f.write(data)
            return
        except SystemExit:
            raise
        except Exception as e:  # a network error: try again
            last = e
            time.sleep(2 + attempt * 3)
    raise SystemExit(f"{entry['url']}: {last}")


def main():
    manifest, root = sys.argv[1], sys.argv[2]
    entries = json.load(open(manifest, encoding="utf-8"))
    with concurrent.futures.ThreadPoolExecutor(8) as pool:
        list(pool.map(lambda e: fetch(e, root), entries))
    index = {"music": [], "sfx": [], "emoji": []}
    for e in entries:
        kind = e["file"].split("/")[0]
        if kind == "emoji":
            index["emoji"].append({"name": e["name"], "emoji": e["emoji"], "file": e["file"]})
        else:
            index[kind].append({k: e[k] for k in ("file", "category", "title", "seconds", "bpm") if k in e})
    for kind, rows in index.items():
        with open(os.path.join(root, kind, "index.json"), "w", encoding="utf-8") as f:
            json.dump(rows, f, ensure_ascii=False, indent=1)
    print(f"{len(entries)} files, {sum(e['bytes'] for e in entries) // 1000000} MB")


if __name__ == "__main__":
    main()
