#!/usr/bin/env python3
"""Make the new stack's store keys (yyc/refactor spec-platform 12.3, 25.4; WP-32) into files, never printing a
value, and print the one `scripts/store-put.py yyc …` command (claude-env) that writes them with their explanations.

  yyc/infra/make-keys.py OUT                  the random keys (pair tokens, pigeon's and hermit's keys, role
                                                 passwords) and the URLs made from them; skips a key the env already
                                                 has (so a re-run never replaces a live value)
  yyc/infra/make-keys.py OUT --derived        the keys made from existing store keys (needs store yyc in the env):
                                                 YYC_PIGEON_LEGACY_VIEW_AS_KEY (YYC_JARVIS_CC_TOKEN), the pg-shared
                                                 forms of YYC_JARVIS_DATABASE_URL, YYC_CLOUDCLI_DATABASE_URL and
                                                 YYC_JUICEFS_META_URL
  yyc/infra/make-keys.py OUT --hosts-meta     YYC_HERMIT_JUICEFS_META_URL: YYC_JUICEFS_HOST_META_URL with the host
                                                 the sandbox hosts dial, the balancer's database port 10.0.0.50:5432
                                                 (deploy/hermit/pg-forward.yaml)
  yyc/infra/make-keys.py OUT --fabric AVIARY  the fabric PKI (AVIARY/fabric/host/make-certs.sh: CA, gateway, host,
                                                 hermit; keys as PKCS#8) and the gateway's WireGuard key

Made elsewhere: YYC_CF_PIGEON_TUNNEL_TOKEN (tofu output tunnel_token_pigeon after the A1 tofu apply),
YYC_OLD_CLUSTER_IMPORT_TOKEN (the old cluster's Secret jarvis/magpie-import-token, read into a file in A1 step 7),
YYC_HERMIT_AWS_* (`fabric-aws iam-key --key-out FILE`, crates/fabric-aws). OUT is created mode 700; every file mode 600. Delete OUT after
the store-put run.
"""
import base64, hashlib, os, secrets, subprocess, sys, urllib.parse

PAIRS = [  # (caller, callee) of platform 3.7; store YYC_TOKEN_<CALLER>_<CALLEE>
    ("PIGEON", "MAGPIE"), ("PIGEON", "HERMIT"), ("PIGEON", "MODEL"),
    ("GATEWAY", "MAGPIE"), ("HERMIT", "MAGPIE"), ("ADMIN", "MAGPIE"),
    ("MAGPIE", "HERMIT"), ("GATEWAY", "HERMIT"), ("TOOLS", "HERMIT"), ("MONITOR", "HERMIT"), ("ADMIN", "HERMIT"),
    ("HERMIT", "EGRESS"), ("ADMIN", "EGRESS"), ("GATEWAY", "TOOLS"),
]
WINDOW = {  # switch window only (A1-A6, P1-A6)
    "YYC_TOKEN_PIGEON_UI": "YYC AI switch window (A1-A6): pigeon's legacy-upstream pair token at the old router "
                           "(x-aviary-lu); sealed into yyc-ai/pigeon (PIGEON_TOKEN_UI) and jarvis/ui-env "
                           "(JARVIS_UI_LU_TOKEN); deleted at teardown",
    "YYC_TOKEN_MAGPIE_IMPORT_UI": "YYC AI switch window (A1-A6): the importer's and the cutover runner's bearer at the "
                                  "old router's switch module; sealed into yyc-ai/magpie-import (MAGPIE_IMPORT_TOKEN_UI) "
                                  "and jarvis/ui-env (JARVIS_UI_SWITCH_TOKEN); deleted at teardown",
    "YYC_TOKEN_MAGPIE_IMPORT": "YYC AI switch window (P1-A6): magpie's bearer at the import Deployment's synchronous "
                               "ingest; sealed into yyc-ai/magpie (MAGPIE_TOKEN_IMPORT) and yyc-ai/magpie-import "
                               "(MAGPIE_IMPORT_ACCEPT_MAGPIE); deleted at teardown",
}
SHARED = "pg-shared.db.svc.cluster.local:5432"
HOSTS_PG = "10.0.0.50:5432"   # balancer yyc-ai-egress's second port -> pg-forward -> db/pg-shared


def b64url32():
    return base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("=")


class Out:
    def __init__(self, d):
        self.d = d
        os.makedirs(d, mode=0o700, exist_ok=True)
        self.keys = {}

    def put(self, key, value, explain):
        if key in os.environ:
            print(f"skip {key}: already in the env (the store has it)")
            return
        p = os.path.join(self.d, key)
        fd = os.open(p, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as f:
            f.write(value)
        self.keys[key] = explain
        print(f"made {key}")

    def command(self):
        if not self.keys:
            print("nothing new")
            return
        args = [f"{k}=file:{os.path.join(self.d, k)}" for k in self.keys]
        for k, e in self.keys.items():
            args += ["--explain", f"{k}={e}"]
        import shlex   # an explanation may hold an apostrophe
        print("\nscripts/store-put.py yyc " + " ".join(shlex.quote(a) for a in args))


def need(name):
    v = os.environ.get(name)
    if not v:
        sys.exit(f"{name} is not in the env: hold store yyc")
    return v


def rehost(url, hostport, sslmode=None):
    """url with its host:port replaced, and its sslmode set when given. sslmode by client: aviary's Rust clients
    (tokio-postgres) refuse `no-verify` and never verify anyway, so `require`; jarvis3's node-postgres verifies the
    certificate under `require` (CloudNativePG's own CA), so `no-verify`"""
    u = urllib.parse.urlsplit(url)
    auth = u.netloc.rsplit("@", 1)[0] + "@" if "@" in u.netloc else ""
    q = urllib.parse.parse_qsl(u.query, keep_blank_values=True)
    if sslmode:
        q = [(k, v) for k, v in q if k != "sslmode"] + [("sslmode", sslmode)]
    return urllib.parse.urlunsplit((u.scheme, auth + hostport, u.path, urllib.parse.urlencode(q), u.fragment))


def random_keys(o):
    for caller, callee in PAIRS:
        o.put(f"YYC_TOKEN_{caller}_{callee}", b64url32(),
              f"YYC AI pair token: {caller.lower()} calling {callee.lower()}; sealed into the caller's and the callee's "
              "Secrets in namespace yyc-ai (DE0CH/yyc-code infra/secrets.txt)")
    for k, e in WINDOW.items():
        o.put(k, b64url32(), e)
    o.put("YYC_PIGEON_VIEW_AS_KEY", b64url32(), "YYC AI pigeon's view-as HMAC key (pigeon spec 3.3); rotation: new "
          "key here, the old one as YYC_PIGEON_VIEW_AS_KEY_PREV for 12 h")
    o.put("YYC_PIGEON_BLOB_KEY", b64url32(), "YYC AI pigeon's blob-link HMAC key (pigeon spec 3.3); rotation: new key "
          "here, the old one as YYC_PIGEON_BLOB_KEY_PREV for 12 h")
    o.put("YYC_HERMIT_TOKEN_KEYS", "1:" + b64url32(), "YYC AI hermit's sandbox and control token keys, kid:key "
          "(hermit spec 6.1)")
    pw = {}
    for role in ("magpie", "hermit", "egress", "tools"):
        k = f"YYC_PG_{role.upper()}_PASSWORD"
        pw[role] = os.environ.get(k) or secrets.token_hex(32)
        o.put(k, pw[role], f"Postgres role {role}'s password (sealed into db/pg-role-{role}); embedded in "
              f"YYC_{role.upper()}_DATABASE_URL (change both)")
    o.put("YYC_MAGPIE_DATABASE_URL", f"postgresql://magpie:{pw['magpie']}@magpie-pg-rw.db.svc.cluster.local:5432/magpie?sslmode=require",
          "YYC AI magpie's database on Cluster magpie-pg (new Talos cluster); its password is YYC_PG_MAGPIE_PASSWORD")
    for role in ("hermit", "egress", "tools"):
        mode = "require" if role == "hermit" else "no-verify"   # rehost()'s note: Rust hermit, node egress and tools
        o.put(f"YYC_{role.upper()}_DATABASE_URL", f"postgresql://{role}:{pw[role]}@{SHARED}/{role}?sslmode={mode}",
              f"YYC AI {role}'s database on Cluster pg through db/pg-shared (bridge B3; after T5 the new primary); its "
              f"password is YYC_PG_{role.upper()}_PASSWORD. Never pg-rw.db: on the new cluster that names T5's replica")


def derived(o):
    cc = need("YYC_JARVIS_CC_TOKEN")
    o.put("YYC_PIGEON_LEGACY_VIEW_AS_KEY",
          base64.urlsafe_b64encode(hashlib.sha256(("jarvis-view-as|" + cc).encode()).digest()).decode().rstrip("="),
          "YYC AI switch window: pigeon's copy of the old router's view-as key, sha256('jarvis-view-as|' + "
          "YYC_JARVIS_CC_TOKEN) base64url; sealed into yyc-ai/pigeon (PIGEON_LEGACY_VIEW_AS_KEY); deleted at teardown")
    for src, dst, mode, who in (
            ("YYC_JARVIS_DATABASE_URL", "YYC_JARVIS_SHARED_DATABASE_URL", "require", "hermit-import and magpie-import"),
            ("YYC_JARVIS_DATABASE_URL", "YYC_JARVIS_SHARED_NODE_DATABASE_URL", "no-verify", "egress-import and tools-import"),
            ("YYC_CLOUDCLI_DATABASE_URL", "YYC_CLOUDCLI_SHARED_DATABASE_URL", "require", "hermit-import and magpie-import")):
        o.put(dst, rehost(need(src), SHARED, mode), f"YYC AI switch window: {src} with host {SHARED} (db/pg-shared on "
              f"the new cluster, bridge B3) and sslmode={mode}: {who}'s read-only reads of the old stack's database; "
              "deleted with the old databases (A6)")
    o.put("YYC_HERMIT_JUICEFS_CSI_META_URL", rehost(need("YYC_JUICEFS_META_URL"), SHARED),
          f"YYC_JUICEFS_META_URL with host {SHARED}: the JuiceFS CSI metaurl of hermit's PV hermit-jfs on the new "
          "cluster (Secret yyc-ai/hermit-juicefs)")


def hosts_meta(o):
    o.put("YYC_HERMIT_JUICEFS_META_URL", rehost(need("YYC_JUICEFS_HOST_META_URL"), HOSTS_PG),
          "YYC AI hermit's JuiceFS metadata URL for the AWS sandbox hosts and hermit's juicefs CLI: "
          f"YYC_JUICEFS_HOST_META_URL with host {HOSTS_PG}, the balancer yyc-ai-egress's database port, which "
          "pg-forward passes on to db/pg-shared (DE0CH/yyc-code yyc/deploy/hermit/pg-forward.yaml)")


def fabric(o, aviary):
    d = os.path.join(o.d, "fabric")
    os.makedirs(d, mode=0o700, exist_ok=True)
    subprocess.run(["bash", os.path.join(aviary, "fabric/host/make-certs.sh"), d], check=True, stdout=subprocess.DEVNULL)

    def p8(leaf):
        return subprocess.run(["openssl", "pkcs8", "-topk8", "-nocrypt", "-in", os.path.join(d, f"{leaf}.key")],
                              check=True, capture_output=True, text=True).stdout

    rd = lambda f: open(os.path.join(d, f)).read()
    o.put("YYC_HERMIT_FABRIC_CA", rd("ca.crt"), "hermit fabric CA certificate (aviary fabric/host/make-certs.sh, hermit 14.5)")
    o.put("YYC_HERMIT_FABRIC_CA_KEY", rd("ca.key"), "hermit fabric CA private key: store only, for new leaves (hermit 14.5)")
    for leaf, key in (("hermit", "CLIENT"), ("host", "HOST"), ("gateway", "GATEWAY")):
        o.put(f"YYC_HERMIT_FABRIC_{key}_CERT", rd(f"{leaf}.crt"), f"hermit fabric {leaf} certificate (hermit 14.5)")
        o.put(f"YYC_HERMIT_FABRIC_{key}_KEY", p8(leaf), f"hermit fabric {leaf} private key, PKCS#8 (hermit 14.5)")
    k = bytearray(secrets.token_bytes(32))
    k[0] &= 248
    k[31] = (k[31] & 127) | 64
    o.put("YYC_HERMIT_GATEWAY_WG_KEY", base64.b64encode(bytes(k)).decode(),
          "WireGuard private key of hermit's gateway hermit-gw (udp 51821; installed on the gateway server by infra/gateway.sh setup --keys)")


def main():
    a = sys.argv[1:]
    if not a or a[0].startswith("-"):
        sys.exit(__doc__)
    o = Out(a[0])
    if "--fabric" in a:
        fabric(o, a[a.index("--fabric") + 1])
    elif "--derived" in a:
        derived(o)
    elif "--hosts-meta" in a:
        hosts_meta(o)
    else:
        random_keys(o)
    o.command()


if __name__ == "__main__":
    main()
