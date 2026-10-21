#!/usr/bin/env python3
"""sandboxd end-to-end and memory tests (docs/SANDBOXD-MEMORY.md "Tests"), with stand-ins for runsc/ip/nft/wg.

Runs two daemons (A on 127.0.0.1, B on 127.0.0.2) as root (mlockall), then:
  functional   every endpoint, 403 for a non-controller certificate, a migration A -> B, restart recovery
  soak         6 sandboxes on A + a slow migration A -> B in flight while every endpoint is hammered at more than
               the maximum concurrency, with hostile clients (slowloris, garbage, oversized bodies, deep JSON); RSS
               sampled throughout must stay flat and under the budget; the allocator must report no stray, overflow
               or failed allocations
Usage: sudo -E python3 tests/soak.py BINARY [--seconds N] [--node ../src/main.ts]   (--node: run the same load
against the Node sandboxd for comparison, RSS only)
"""
import argparse, http.client, json, os, random, shutil, signal, socket, ssl, stat, subprocess, sys, tempfile, threading, time

HERE = os.path.dirname(os.path.abspath(__file__))
FAKE = os.path.join(HERE, "fake")
DIGEST = "c" * 64
GOOD_KEY = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbA="


def sh(*a, **k):
    return subprocess.run(a, check=True, capture_output=True, text=True, **k)


def make_pki(d):
    sh("bash", os.path.join(HERE, "..", "..", "host", "make-certs.sh"), d, "hub", "controller")
    # a client certificate from the same CA with another CN: must get 403
    sh("openssl", "req", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", f"{d}/intruder.key", "-out", f"{d}/intruder.csr", "-subj", "/CN=intruder")
    open(f"{d}/intruder.ext", "w").write("extendedKeyUsage=clientAuth\nbasicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature\n")
    sh("openssl", "x509", "-req", "-in", f"{d}/intruder.csr", "-CA", f"{d}/ca.crt", "-CAkey", f"{d}/ca.key", "-CAcreateserial", "-days", "30", "-extfile", f"{d}/intruder.ext", "-out", f"{d}/intruder.crt")


class Daemon:
    def __init__(self, base, name, ip, binary, pki, env_extra=None, limits=None, node=None):
        self.ip, self.base = ip, f"{base}/{name}"
        self.root = f"{self.base}/root"
        img = f"{self.root}/var/lib/sbx/images/{DIGEST}"
        os.makedirs(f"{img}/rootfs", exist_ok=True)
        json.dump({"config": {"Env": ["PATH=/usr/local/bin:/usr/bin", "LANG=C.UTF-8", "HOME=/root"]}}, open(f"{img}/config.json", "w"))
        open(f"{img}/.done", "w").close()
        lim = {"cgroups": False, "mlock": True}
        lim.update(limits or {})
        self.cfg = f"{self.base}/host.json"
        json.dump({"name": name, "role": "burst", "wgAddress": ip, "hostCidr": "127.0.0.0/24", "apiPort": 17443,
                   "hub": {"publicKey": GOOD_KEY, "endpoint": "1.2.3.4:51820", "allowedIps": ["10.201.0.1/32"]},
                   "tls": {"ca": f"{pki}/ca.crt", "cert": f"{pki}/hub.crt", "key": f"{pki}/hub.key"}, "limits": lim}, open(self.cfg, "w"))
        self.env = dict(SBX_ROOT=self.root, SBX_CONFIG=self.cfg, SBX_RUNSC=f"{FAKE}/runsc", SBX_FAKE=f"{self.base}/fake",
                        PATH=f"{FAKE}:{os.environ['PATH']}", **(env_extra or {}))
        self.binary, self.node, self.proc = binary, node, None
        self.log = open(f"{self.base}/log", "a")

    def start(self):
        cmd = [self.binary] if not self.node else ["node", "--disable-warning=ExperimentalWarning", self.node]
        self.proc = subprocess.Popen(cmd, env=self.env, stdout=self.log, stderr=self.log)
        for _ in range(200):
            try:
                socket.create_connection((self.ip, 17443), timeout=0.2).close()
                return self
            except OSError:
                if self.proc.poll() is not None:
                    raise SystemExit(f"daemon {self.ip} exited: {open(self.base + '/log').read()[-2000:]}")
                time.sleep(0.05)
        raise SystemExit("daemon did not listen")

    def stop(self, sig=signal.SIGTERM):
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(sig)
            self.proc.wait(10)

    def rss(self):
        try:
            for l in open(f"/proc/{self.proc.pid}/status"):
                if l.startswith("VmRSS:"):
                    return int(l.split()[1]) * 1024
        except OSError:
            return 0
        return 0


class Client:
    def __init__(self, pki, cert="controller"):
        c = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        c.minimum_version = ssl.TLSVersion.TLSv1_3
        c.load_verify_locations(f"{pki}/ca.crt")
        c.load_cert_chain(f"{pki}/{cert}.crt", f"{pki}/{cert}.key")
        c.check_hostname = False
        self.ctx = c

    def conn(self, ip):
        return http.client.HTTPSConnection(ip, 17443, context=self.ctx, timeout=60)

    def call(self, ip, method, path, body=None, conn=None, raw=None):
        c = conn or self.conn(ip)
        data = raw if raw is not None else (json.dumps(body) if body is not None else None)
        h = {"content-type": "application/json"} if data is not None else {}
        c.request(method, path, body=data, headers=h)
        r = c.getresponse()
        s = r.read()
        if not conn:
            c.close()
        try:
            return r.status, json.loads(s) if s else None
        except ValueError:
            return r.status, s


def spec(n, ip, mem=1 << 30):
    return {"name": f"sbx-{n}", "ip": ip, "image": DIGEST, "imageRef": "ghcr.io/de0ch/jarvis3-sandbox:test",
            "env": {"HTTP_PROXY": "http://sbx:token@10.201.1.1:3128", "JARVIS_EXEC_TOKEN": "e" * 40, "NO_PROXY": "localhost"},
            "cpu": 1, "memory": mem, "cpuFeatures": "fpu,sse2"}


def check(cond, what):
    if not cond:
        raise SystemExit(f"FAIL: {what}")
    print(f"ok  {what}")


def functional(A, B, cl, intruder):
    s, h = cl.call(A.ip, "GET", "/v1/host")
    check(s == 200 and h["role"] == "burst" and h["images"] == [DIGEST] and h["cpuFeatures"] == "fpu,sse2,avx", "GET /v1/host")
    check(h["sandboxd"]["memory"]["budget"] > 0 and h["peers"][0]["allowedIps"] == ["10.201.0.1/32"], "host: memory + peers")
    check(intruder.call(A.ip, "GET", "/v1/host") == (403, {"error": "not the controller"}), "403 for another client certificate")
    s, r = cl.call(A.ip, "POST", "/v1/sandboxes", spec("f1", "10.200.0.11"))
    check(s == 200 and r["name"] == "sbx-f1" and r["state"] == "running" and r["memory"] == 1 << 30, f"create {s} {r}")
    check(cl.call(A.ip, "POST", "/v1/sandboxes", spec("f1", "10.200.0.11"))[0] == 200, "create is idempotent")
    check(cl.call(A.ip, "POST", "/v1/sandboxes", spec("f2", "10.200.0.11"))[0] == 409, "ip in use: 409")
    bundle = json.load(open(f"{A.root}/var/lib/sbx/bundles/sbx-f1/config.json"))
    check("HOME=/home/sandbox" in bundle["process"]["env"] and "LANG=C.UTF-8" in bundle["process"]["env"], "bundle env")
    s, sp = cl.call(A.ip, "GET", "/v1/sandboxes/sbx-f1/spec")
    check(s == 200 and sp["env"]["JARVIS_EXEC_TOKEN"] == "e" * 40 and sp["imageRef"].endswith(":test"), "spec")
    check(cl.call(A.ip, "GET", "/v1/sandboxes/sbx-nope/spec")[0] == 404, "spec of a missing one: 404")
    check(cl.call(A.ip, "GET", "/v1/sandboxes/BAD/spec")[0] == 400, "bad name: 400")
    check(cl.call(A.ip, "DELETE", "/v1/sandboxes/sbx-nope") == (200, {"deleted": "sbx-nope"}), "delete a missing one")
    check(cl.call(A.ip, "POST", "/v1/sandboxes", raw="{")[0] == 400, "bad JSON: 400")
    check(cl.call(A.ip, "POST", "/v1/sandboxes", dict(spec("x", "10.200.0.12"), cpu=0))[0] == 400, "bad spec: 400")
    check(cl.call(A.ip, "PUT", "/v1/egress", {"targets": ["10.244.0.5"]})[0] == 400, "egress on a burst host: 400")
    check(cl.call(A.ip, "PUT", "/v1/peers", {"peers": [{"publicKey": GOOD_KEY, "allowedIps": ["10.201.0.1/32"]}]})[0] == 200, "PUT peers")
    check(cl.call(A.ip, "PUT", "/v1/peers", {"peers": [{"publicKey": "short"}]})[0] == 400, "PUT peers with a bad key: 400")
    check(cl.call(A.ip, "POST", "/v1/images", {"digest": DIGEST}) == (200, {"digest": DIGEST}), "images: have it")
    check(cl.call(A.ip, "GET", "/v1/nope")[0] == 404, "unknown path: 404")
    check(cl.call(A.ip, "POST", "/v1/sandboxes", raw="x" * (70 << 10))[0] == 413, "body too large: 413")
    # migration A -> B
    s, sp = cl.call(A.ip, "GET", "/v1/sandboxes/sbx-f1/spec")
    # the scratch filesystem moves with it: files, hard links, mtimes, overlayfs whiteouts (0/0 character devices)
    # and opaque directories (trusted.overlay.* xattrs), /tmp
    sa, sb = f"{A.root}/var/lib/sbx/scratch/sbx-f1", f"{B.root}/var/lib/sbx/scratch/sbx-f1"
    os.makedirs(f"{sa}/upper/etc/opq", exist_ok=True)
    open(f"{sa}/upper/etc/new.conf", "w").write("x" * 5000)
    os.link(f"{sa}/upper/etc/new.conf", f"{sa}/upper/etc/hard.conf")
    os.utime(f"{sa}/upper/etc/new.conf", ns=(1_700_000_000_123_456_789, 1_700_000_000_123_456_789))
    os.mknod(f"{sa}/upper/etc/gone", 0o600 | stat.S_IFCHR, os.makedev(0, 0))
    os.setxattr(f"{sa}/upper/etc/opq", "trusted.overlay.opaque", b"y")
    open(f"{sa}/tmp/t.txt", "w").write("tmp")
    s, rx = cl.call(B.ip, "POST", "/v1/sandboxes/sbx-f1/receive", sp)
    check(s == 200 and rx["address"] == B.ip and 7100 <= rx["port"] <= 7199, "receive")
    check(cl.call(A.ip, "POST", "/v1/sandboxes/sbx-f1/send", {"to": "10.9.9.9:1"})[0] == 400, "send to outside the overlay: 400")
    s, r = cl.call(A.ip, "POST", "/v1/sandboxes/sbx-f1/send", {"to": f"{B.ip}:{rx['port']}", "downtimeMs": 100, "maxDowntimeMs": 500})
    check(s == 200 and r["ms"] >= 0, "send")
    s, r = cl.call(B.ip, "GET", "/v1/sandboxes/sbx-f1/receive?wait=20000")
    check(s == 200 and r["state"] == "running" and r["stats"]["bytes"] > 0, f"receive result {r}")
    st = os.stat(f"{sb}/upper/etc/new.conf"); g = os.stat(f"{sb}/upper/etc/gone")
    check(open(f"{sb}/upper/etc/new.conf").read() == "x" * 5000 and st.st_mtime_ns == 1_700_000_000_123_456_789
          and st.st_ino == os.stat(f"{sb}/upper/etc/hard.conf").st_ino and stat.S_ISCHR(g.st_mode) and g.st_rdev == 0
          and os.getxattr(f"{sb}/upper/etc/opq", "trusted.overlay.opaque") == b"y"
          and open(f"{sb}/tmp/t.txt").read() == "tmp" and not os.path.exists(sa), "scratch moved (files, links, times, whiteouts, xattrs)")
    check([x["name"] for x in cl.call(A.ip, "GET", "/v1/sandboxes")[1]["sandboxes"]] == [], "gone from A")
    check([x["name"] for x in cl.call(B.ip, "GET", "/v1/sandboxes")[1]["sandboxes"]] == ["sbx-f1"], "running on B")
    # a send that fails before the switch: 409, still there
    B.env["FAKE_SEND_FAIL"] = "1"
    B.stop(); B.start()
    s, sp = cl.call(B.ip, "GET", "/v1/sandboxes/sbx-f1/spec")
    check(s == 200, "B restarted: its sandbox is still there (state from files)")
    s, rx = cl.call(A.ip, "POST", "/v1/sandboxes/sbx-f1/receive", sp)
    s, r = cl.call(B.ip, "POST", "/v1/sandboxes/sbx-f1/send", {"to": f"{A.ip}:{rx['port']}"})
    check(s == 409 and "still runs here" in r["error"], "send that won't converge: 409")
    check(cl.call(A.ip, "DELETE", "/v1/sandboxes/sbx-f1")[0] == 200, "target cleans up the failed receive")
    s, r = cl.call(A.ip, "GET", "/v1/sandboxes/sbx-f1/receive?wait=5000")
    check(r["state"] == "failed", f"receive result after delete: {r}")
    del B.env["FAKE_SEND_FAIL"]
    B.stop(); B.start()
    check(cl.call(B.ip, "DELETE", "/v1/sandboxes/sbx-f1")[0] == 200, "delete")


def hostile(ip, pki, stop):
    """Clients a daemon must survive: slowloris, garbage, half handshakes, deep JSON."""
    held = []
    while not stop.is_set():
        k = random.randrange(5)
        try:
            s = socket.create_connection((ip, 17443), timeout=5)
            if k == 0:
                held.append(s)  # connect and say nothing (holds a worker until its timeout)
                if len(held) > 2:
                    held.pop(0).close()
                continue
            if k == 1:
                s.sendall(os.urandom(random.randrange(1, 20000)))
            elif k == 2:
                s.sendall(b"\x16\x03\x01\xff\xff" + os.urandom(3000))  # a ClientHello claiming 64 KiB, then nothing
                time.sleep(0.5)
            elif k == 3:
                s.sendall(b"GET / HTTP/1.1\r\n\r\n")
            s.close()
        except OSError:
            pass
        time.sleep(0.02)
    for s in held:
        s.close()


def soak(A, B, cl, intruder, seconds, pki, budget_rss):
    print(f"soak: {seconds}s; 6 sandboxes on A, a slow migration A -> B, hammering every endpoint")
    for i in range(6):
        s, _ = cl.call(A.ip, "POST", "/v1/sandboxes", spec(f"s{i}", f"10.200.1.{i + 1}"))
        assert s == 200
    stop = threading.Event()
    counts, errors = {}, []
    lock = threading.Lock()

    def migrate_loop():
        # a 20 s migration of sbx-s0 back and forth, all the time
        src, dst = A, B
        while not stop.is_set():
            s, sp = cl.call(src.ip, "GET", "/v1/sandboxes/sbx-s0/spec")
            s, rx = cl.call(dst.ip, "POST", "/v1/sandboxes/sbx-s0/receive", sp)
            if s != 200:
                errors.append(f"receive {s} {rx}"); return
            s, r = cl.call(src.ip, "POST", "/v1/sandboxes/sbx-s0/send", {"to": f"{dst.ip}:{rx['port']}"})
            s2, r2 = cl.call(dst.ip, "GET", "/v1/sandboxes/sbx-s0/receive?wait=60000")
            if s != 200 or r2.get("state") != "running":
                errors.append(f"migration {s} {r} {r2}"); return
            with lock:
                counts["migrations"] = counts.get("migrations", 0) + 1
            src, dst = dst, src

    big_env = {f"V{i}": "y" * 900 for i in range(60)}  # ~56 KiB body, under the 64 KiB limit

    def hammer(n):
        conn = None
        rnd = random.Random(n)
        while not stop.is_set():
            try:
                if conn is None or rnd.random() < 0.2:
                    if conn:
                        conn.close()
                    conn = cl.conn(A.ip)
                k = rnd.randrange(12)
                if k == 0:
                    r = cl.call(A.ip, "GET", "/v1/host", conn=conn)
                elif k == 1:
                    r = cl.call(A.ip, "GET", "/v1/sandboxes", conn=conn)
                elif k == 2:
                    r = cl.call(A.ip, "GET", f"/v1/sandboxes/sbx-s{rnd.randrange(1, 6)}/spec", conn=conn)
                elif k == 3:
                    r = cl.call(A.ip, "PUT", "/v1/peers", {"peers": [{"publicKey": GOOD_KEY, "allowedIps": ["10.201.0.1/32"]}]}, conn=conn)
                elif k == 4:
                    name = f"t{n}"
                    r = cl.call(A.ip, "POST", "/v1/sandboxes", dict(spec(name, f"10.200.2.{n + 1}"), env=big_env), conn=conn)
                    cl.call(A.ip, "DELETE", f"/v1/sandboxes/sbx-{name}", conn=conn)
                elif k == 5:
                    r = cl.call(A.ip, "POST", "/v1/sandboxes", raw="[" * 5000 + "]" * 5000, conn=conn)
                elif k == 6:
                    r = cl.call(A.ip, "POST", "/v1/sandboxes", raw="x" * (200 << 10), conn=conn)
                    conn.close(); conn = None
                elif k == 7:
                    r = cl.call(A.ip, "POST", "/v1/images", {"digest": DIGEST}, conn=conn)
                elif k == 8:
                    r = cl.call(A.ip, "GET", "/v1/sandboxes/sbx-s0/receive?wait=10", conn=conn)
                elif k == 9:
                    r = intruder.call(A.ip, "GET", "/v1/host")
                elif k == 10:
                    r = cl.call(A.ip, "PUT", "/v1/egress", {"targets": ["10.0.0.1"] * 100}, conn=conn)
                else:
                    r = cl.call(A.ip, "DELETE", f"/v1/sandboxes/sbx-missing{n}", conn=conn)
                with lock:
                    counts[r[0]] = counts.get(r[0], 0) + 1
            except (OSError, http.client.HTTPException) as e:
                conn = None
                with lock:
                    counts["conn-error"] = counts.get("conn-error", 0) + 1

    threads = [threading.Thread(target=hammer, args=(i,), daemon=True) for i in range(32)]
    threads += [threading.Thread(target=hostile, args=(A.ip, pki, stop), daemon=True) for _ in range(4)]
    threads.append(threading.Thread(target=migrate_loop, daemon=True))
    rss = []
    t0 = time.time()
    for t in threads:
        t.start()
    while time.time() - t0 < seconds:
        rss.append((round(time.time() - t0, 2), A.rss(), B.rss()))
        time.sleep(0.25)
        if A.proc.poll() is not None or B.proc.poll() is not None:
            raise SystemExit("FAIL: a daemon died during the soak")
    stop.set()
    for t in threads:
        t.join(70)
    check(not errors, f"migrations during the soak: {counts.get('migrations', 0)} done, errors {errors}")
    print("responses:", dict(sorted(counts.items(), key=str)))
    ra = [x[1] for x in rss]
    rb = [x[2] for x in rss]
    print(f"RSS A: min {min(ra) / 2**20:.2f} max {max(ra) / 2**20:.2f} MiB; B: min {min(rb) / 2**20:.2f} max {max(rb) / 2**20:.2f} MiB; bound {budget_rss / 2**20:.2f} MiB")
    s, h = cl.call(A.ip, "GET", "/v1/host")
    m = h["sandboxd"]["memory"]
    print("allocator:", m)
    check(max(ra) <= budget_rss and max(rb) <= budget_rss, "RSS under the documented bound")
    check(max(ra) - min(ra) <= 64 << 10, "RSS flat (mlockall: everything resident from start)")
    check(m["stray"] == 0 and m["overflow"] == 0 and m["failed"] == 0, "no allocation outside TLS, no arena overflow, no failure")
    check(m["tlsPeak"] < m["tlsArenaSize"] - (200 << 10), "TLS arena peak below arena - headroom")
    on_b = cl.call(B.ip, "GET", "/v1/sandboxes")[1]["sandboxes"]
    check(len(h["sandboxes"]) + len(on_b) == 6, "the 6 sandboxes are all there after the soak (A + B)")
    check(m["apiStackPeak"] < m["apiStack"] and m["otherStackPeak"] < m["otherStack"], "stack peaks within the stack sizes")
    json.dump({"rss": rss, "counts": {str(k): v for k, v in counts.items()}, "memory": m}, open(os.path.join(os.path.dirname(A.base), "soak.json"), "w"))
    return rss


def node_soak(binary_node, base, pki, seconds):
    """The same load (without the migration loop) against the Node sandboxd, for the RSS comparison."""
    A = Daemon(base, "node", "127.0.0.3", None, pki, node=binary_node)
    # Node's sandboxd has no SBX_ROOT: it uses /var/lib/sbx etc. on this machine
    os.makedirs(f"/var/lib/sbx/images/{DIGEST}/rootfs", exist_ok=True)
    shutil.copy(f"{A.root}/var/lib/sbx/images/{DIGEST}/config.json", f"/var/lib/sbx/images/{DIGEST}/config.json")
    open(f"/var/lib/sbx/images/{DIGEST}/.done", "w").close()
    A.start()
    cl = Client(pki)
    idle = A.rss()
    for i in range(6):
        cl.call(A.ip, "POST", "/v1/sandboxes", spec(f"n{i}", f"10.200.3.{i + 1}"))
    stop = threading.Event()

    def hammer(n):
        while not stop.is_set():
            try:
                k = n % 4
                if k == 0:
                    cl.call(A.ip, "GET", "/v1/host")
                elif k == 1:
                    cl.call(A.ip, "GET", "/v1/sandboxes")
                elif k == 2:
                    cl.call(A.ip, "POST", "/v1/sandboxes", raw="x" * (200 << 10))
                else:
                    cl.call(A.ip, "PUT", "/v1/peers", {"peers": [{"publicKey": GOOD_KEY, "allowedIps": ["10.201.0.1/32"]}]})
            except Exception:
                pass

    ts = [threading.Thread(target=hammer, args=(i,), daemon=True) for i in range(32)]
    for t in ts:
        t.start()
    peak = idle
    t0 = time.time()
    while time.time() - t0 < seconds:
        peak = max(peak, A.rss())
        time.sleep(0.25)
    stop.set()
    for i in range(6):
        cl.call(A.ip, "DELETE", f"/v1/sandboxes/sbx-n{i}")
    A.stop()
    print(f"Node sandboxd RSS: idle {idle / 2**20:.1f} MiB, peak {peak / 2**20:.1f} MiB")
    return idle, peak


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("binary")
    ap.add_argument("--seconds", type=int, default=60)
    ap.add_argument("--node")
    ap.add_argument("--budget-binary")
    a = ap.parse_args()
    base = tempfile.mkdtemp(prefix="sbx-soak-")
    pki = f"{base}/pki"
    make_pki(pki)
    budget = subprocess.run([a.budget_binary or a.binary, "budget"], capture_output=True, text=True).stdout
    rss_bound = float([l for l in budget.splitlines() if "resident (RSS) bound" in l][0].split()[-2]) * 2**20
    cl, intruder = Client(pki), Client(pki, "intruder")
    A = Daemon(base, "a", "127.0.0.1", a.binary, pki, {"FAKE_MIGRATE_MS": "200"}).start()
    B = Daemon(base, "b", "127.0.0.2", a.binary, pki, {"FAKE_MIGRATE_MS": "200"}).start()
    try:
        idle = (A.rss(), B.rss())
        print(f"idle RSS: A {idle[0] / 2**20:.2f} MiB, B {idle[1] / 2**20:.2f} MiB (bound {rss_bound / 2**20:.2f} MiB)")
        functional(A, B, cl, intruder)
        # the soak's migration takes ~20 s each way
        for d in (A, B):
            d.env["FAKE_MIGRATE_MS"] = "20000"
            d.env["FAKE_MIGRATE_BYTES"] = str(64 << 20)
            d.stop(); d.start()
        soak(A, B, cl, intruder, a.seconds, pki, rss_bound)
        if a.node:
            node_soak(a.node, base, pki, min(a.seconds, 60))
        print("ALL OK")
    finally:
        A.stop(); B.stop()
        print("logs and results in", base)


if __name__ == "__main__":
    main()
