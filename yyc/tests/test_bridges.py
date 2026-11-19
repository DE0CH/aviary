#!/usr/bin/env python3
"""yyc/infra/bridges.sh against a fake Hetzner Cloud API, fake kubectls and a fake box that runs a REAL HAProxy
(WP-60's harness case; DESIGN 15.5). Stdlib only plus the haproxy binary (the yyc-ai.yml job check installs it); no
cloud, no cluster.

The fake box is this machine: `BRIDGES_SSH` runs each remote command in local bash, `BRIDGES_BOX_ROOT` puts the box's
paths under a temp folder, a fake systemctl starts and reloads haproxy. The bridges are on 127.0.0.2 and 127.0.0.3
and the target "nodes" are TCP listeners on 127.0.0.1x (a listener only where a callee pod runs, as with
externalTrafficPolicy Local), so traffic and health checks really flow.

  - `render` of the real bridges.json: one `listen` per service on 10.0.255.1/.2, every k3s (talos) server's private
    IP at the NodePort, yyc-nat1 excluded, sourced from the bridge's address, check every 3 s, timeout 2 s, down after
    3 failures; `haproxy -c` accepts it;
  - `up` creates the server with no public IP, on network yyc, label yyc-cluster=bridge, cloud-init with the default
    route and resolver, sets both alias IPs; traffic through a bridge reaches a callee node from the bridge's address;
    a second `up` changes nothing (no API action, no reload);
  - `check` is green with every callee node UP, red when a node that runs a callee pod is no target (label gone, or a
    new server not yet in the config), red when such a node is DOWN, red with no ready callee pod;
  - `down B5` removes that service only; `down yyc-bridge-old` removes the bridge and its alias IP; the last bridge's
    down deletes the server; `check` is then red; `down` with no argument is refused.
"""
import json, os, shutil, signal, socket, subprocess, tempfile, threading, time, unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SH = os.path.join(ROOT, "yyc", "infra", "bridges.sh")
DEF = os.path.join(ROOT, "yyc", "infra", "bridges.json")
NET = 12754993
HAPROXY = shutil.which("haproxy") or shutil.which("haproxy", path="/usr/sbin:/usr/local/sbin")


def selector_match(selector, labels):
    """Hetzner's label selector: k=v, k!=v (the key must exist), k, !k; comma = and."""
    for term in selector.split(","):
        if "!=" in term:
            k, v = term.split("!=", 1)
            if k not in labels or labels[k] == v:
                return False
        elif "=" in term:
            k, v = term.split("=", 1)
            if labels.get(k) != v:
                return False
        elif term.startswith("!"):
            if term[1:] in labels:
                return False
        elif term not in labels:
            return False
    return True


class Fake:
    """The parts of the Hetzner Cloud API that bridges.sh uses."""

    def __init__(self):
        self.servers, self.actions, self.log, self.violations, self.created = [], {}, [], [], []
        self.next_id = 1000
        self.box_ip = "127.0.0.50"
        self.on_delete = lambda s: None

    def nid(self):
        self.next_id += 1
        return self.next_id

    def action(self, cmd):
        a = {"id": self.nid(), "command": cmd, "status": "success", "error": None}
        self.actions[a["id"]] = a
        self.log.append(cmd)
        return {"action": a}

    def add(self, sid, name, labels, ip, **kw):
        s = {"id": sid, "name": name, "labels": labels, "status": "running", "server_type": {"name": kw.get("type", "cx23")},
             "location": {"name": kw.get("location", "fsn1")},
             "public_net": {"ipv4": None, "ipv6": None if kw.get("no_v6") else {"ip": "2a01:4f8::/64"}},
             "private_net": [{"network": NET, "ip": ip, "alias_ips": []}]}
        self.servers.append(s)
        return s

    def by_name(self, name):
        return next((s for s in self.servers if s["name"] == name), None)


def handler(fake):
    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def reply(self, code, body):
            data = json.dumps(body).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def body(self):
            n = int(self.headers.get("Content-Length") or 0)
            return json.loads(self.rfile.read(n) or b"{}")

        def authed(self):
            if self.headers.get("Authorization") != "Bearer test-token":
                self.reply(401, {"error": {"code": "unauthorized"}})
                return False
            return True

        def do_GET(self):
            if not self.authed():
                return
            u = urlparse(self.path); q = parse_qs(u.query); p = u.path.split("/")[1:]
            if p == ["networks"]:
                return self.reply(200, {"networks": [{"id": NET, "name": "yyc"}] if q.get("name") == ["yyc"] else []})
            if p == ["servers"]:
                ss = fake.servers
                if "label_selector" in q:
                    ss = [s for s in ss if selector_match(q["label_selector"][0], s["labels"])]
                if "name" in q:
                    ss = [s for s in ss if s["name"] == q["name"][0]]
                return self.reply(200, {"servers": ss})
            if p[0] == "actions":
                return self.reply(200, {"action": fake.actions[int(p[1])]})
            self.reply(404, {"error": {"code": "not_found"}})

        def do_POST(self):
            if not self.authed():
                return
            p = urlparse(self.path).path.split("/")[1:]; b = self.body()
            if p == ["servers"]:
                fake.created.append(b)
                if b.get("public_net") != {"enable_ipv4": False, "enable_ipv6": False}:
                    fake.violations.append("created with a public IP")
                if b.get("networks") != [NET]:
                    fake.violations.append("created without network yyc")
                s = fake.add(fake.nid(), b["name"], b.get("labels", {}), fake.box_ip, type=b["server_type"],
                             location=b["location"], no_v6=True)
                s["public_net"] = {"ipv4": None, "ipv6": None}
                out = {"server": s, **fake.action("create_server")}
                out["next_actions"] = [fake.action("attach_to_network")["action"]]
                return self.reply(201, out)
            if len(p) == 4 and p[0] == "servers" and p[3] == "change_alias_ips":
                s = next((s for s in fake.servers if s["id"] == int(p[1])), None)
                if s is None:
                    return self.reply(404, {"error": {"code": "not_found"}})
                pn = next(n for n in s["private_net"] if n["network"] == b["network"])
                pn["alias_ips"] = list(b["alias_ips"])
                return self.reply(201, fake.action("change_alias_ips"))
            self.reply(404, {"error": {"code": "not_found"}})

        def do_PUT(self):
            if not self.authed():
                return
            p = urlparse(self.path).path.split("/")[1:]; b = self.body()
            s = next((s for s in fake.servers if s["id"] == int(p[1])), None)
            s["labels"] = b["labels"]
            fake.log.append("update_server")
            self.reply(200, {"server": s})

        def do_DELETE(self):
            if not self.authed():
                return
            p = urlparse(self.path).path.split("/")[1:]
            s = next((s for s in fake.servers if s["id"] == int(p[1])), None)
            fake.servers = [x for x in fake.servers if x["id"] != int(p[1])]
            if s:
                fake.on_delete(s)
            self.reply(200, fake.action("delete_server"))

    return H


FAKE_KUBECTL = r'''#!/usr/bin/env python3
import json, sys
sys.stdin.read()  # as ssh does: a kubectl through ssh reads stdin, so bridges.sh must not hand it its loop's input
pods = json.load(open(sys.argv[1]))
a = sys.argv[2:]
ns = a[a.index("-n") + 1]; sel = a[a.index("-l") + 1]
k, v = sel.split("=", 1)
items = [{"metadata": {"name": p["name"]}, "spec": {"nodeName": p["node"]},
          "status": {"conditions": [{"type": "Ready", "status": "True" if p.get("ready", True) else "False"}]}}
         for p in pods.get(ns, []) if p["labels"].get(k) == v]
print(json.dumps({"items": items}))
'''

FAKE_SSH = r'''#!/bin/bash
# the fake box: `fake-ssh <ip> <command...>` runs the command here, haproxy and the fake systemctl on PATH
shift
export PATH="$(dirname "$0")/box:/usr/sbin:/usr/local/sbin:$PATH" BOX="$BRIDGES_BOX_ROOT"
exec bash -c "$*"
'''

FAKE_SYSTEMCTL = r'''#!/bin/bash
# the fake box's systemctl: haproxy runs for real (daemon, pid file), every other unit is a no-op
echo "$*" >> "$BOX/systemctl.log"
q=""; [ "${2:-}" = -q ] && q=1
unit=${!#}
[ "$unit" = haproxy ] || exit 0
pid="$BOX/run/haproxy.pid" cfg="$BOX/etc/haproxy/haproxy.cfg"
alive() { [ -s "$pid" ] && kill -0 "$(head -1 "$pid")" 2>/dev/null; }
case "$1" in
  enable) ;;
  is-active) alive ;;
  reload-or-restart|restart)
    if alive; then haproxy -D -f "$cfg" -p "$pid.new" -sf $(cat "$pid") && mv "$pid.new" "$pid"
    else haproxy -D -f "$cfg" -p "$pid"; fi ;;
  stop) alive && kill $(cat "$pid"); rm -f "$pid" ;;
esac
'''


def write(path, text, mode=None):
    with open(path, "w") as f:
        f.write(text)
    if mode:
        os.chmod(path, mode)


def read(path):
    with open(path) as f:
        return f.read()


def free_port(ips):
    """a TCP port free on every address in ips"""
    for _ in range(200):
        s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close()
        try:
            socks = []
            for ip in ips:
                t = socket.socket(); t.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); t.bind((ip, port)); socks.append(t)
            for t in socks:
                t.close()
            return port
        except OSError:
            for t in socks:
                t.close()
    raise RuntimeError("no free port")


class Node:
    """a NodePort on one node: answers with the node's name, records the peer's address"""

    def __init__(self, name, ip, port):
        self.name, self.peers = name, set()
        self.sock = socket.socket(); self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind((ip, port)); self.sock.listen(64)
        threading.Thread(target=self.serve, daemon=True).start()

    def serve(self):
        while True:
            try:
                c, addr = self.sock.accept()
            except OSError:
                return
            self.peers.add(addr[0])
            try:
                c.sendall(self.name.encode() + b"\n")
            except OSError:
                pass
            c.close()

    def close(self):
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.sock.close()


def ask(ip, port):
    with socket.create_connection((ip, port), timeout=3) as c:
        return c.makefile().readline().strip()


class Base(unittest.TestCase):
    def setUp(self):
        self.fake = Fake()
        self.srv = ThreadingHTTPServer(("127.0.0.1", 0), handler(self.fake))
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.dir = tempfile.mkdtemp(prefix="bridges-")
        kc = os.path.join(self.dir, "kubectl.py"); write(kc, FAKE_KUBECTL)
        self.old_pods = os.path.join(self.dir, "old.json"); self.new_pods = os.path.join(self.dir, "new.json")
        bindir = os.path.join(self.dir, "bin"); os.makedirs(os.path.join(bindir, "box"))
        write(os.path.join(bindir, "kubectl"), f"#!/bin/sh\nexec python3 {kc} {self.new_pods} \"$@\"\n", 0o755)
        write(os.path.join(bindir, "fake-ssh"), FAKE_SSH, 0o755)
        write(os.path.join(bindir, "box", "systemctl"), FAKE_SYSTEMCTL, 0o755)
        self.box = os.path.join(self.dir, "box"); os.makedirs(self.box)
        self.fake.on_delete = lambda s: self.stop_haproxy()
        self.env = {**os.environ, "PATH": bindir + os.pathsep + os.environ["PATH"],
                    "HCLOUD_ENDPOINT": f"http://127.0.0.1:{self.srv.server_port}", "YYC_HETZNER_API": "test-token",
                    "BRIDGES_OLD_KUBECTL": f"python3 {kc} {self.old_pods}", "BRIDGES_NEW_KUBECONFIG": "/dev/null",
                    "BRIDGES_SSH": os.path.join(bindir, "fake-ssh"), "BRIDGES_BOX_ROOT": self.box}
        self.env.pop("CUTOVER_OLD", None)
        self.nodes = []

    def tearDown(self):
        self.stop_haproxy()
        for n in self.nodes:
            n.close()
        self.srv.shutdown()
        self.srv.server_close()
        shutil.rmtree(self.dir, ignore_errors=True)

    def stop_haproxy(self):
        pid = os.path.join(self.box, "run", "haproxy.pid")
        if os.path.exists(pid):
            for line in read(pid).split():
                try:
                    os.kill(int(line), signal.SIGTERM)
                except (ProcessLookupError, ValueError):
                    pass
            os.remove(pid)
        for p in ("etc", "run", "usr"):
            shutil.rmtree(os.path.join(self.box, p), ignore_errors=True)

    @staticmethod
    def p(name, sel, node, ready=True):
        k, v = sel.split("=")
        return {"name": name, "labels": {k: v}, "node": node, "ready": ready}

    def pods(self, old=None, new=None):
        if old is not None:
            write(self.old_pods, json.dumps(old))
        if new is not None:
            write(self.new_pods, json.dumps(new))

    def run_sh(self, *a):
        r = subprocess.run(["bash", SH, *a], env=self.env, capture_output=True, text=True, timeout=180)
        return r.returncode, r.stdout + r.stderr


class Render(Base):
    """the real bridges.json"""

    def test_render_real_definition(self):
        f = self.fake
        f.add(1, "yyc-master1", {"yyc-cluster": "k3s", "role": "master"}, "10.0.0.3")
        f.add(2, "yyc-pool-large-worker1", {"yyc-cluster": "k3s", "role": "worker"}, "10.0.0.10")
        f.add(11, "yyc-talos-cp1", {"yyc-cluster": "talos", "role": "control"}, "10.0.0.21")
        f.add(12, "yyc-talos-w1", {"yyc-cluster": "talos"}, "10.0.0.31", no_v6=True)
        f.add(13, "yyc-nat1", {"yyc-cluster": "talos", "role": "nat"}, "10.0.0.30")
        self.env.pop("BRIDGES_BOX_ROOT")
        code, out = self.run_sh("render")
        self.assertEqual(code, 0, out)
        sec = {}
        for block in out.split("\nlisten ")[1:]:
            name, *lines = block.strip().split("\n")
            sec[name] = [l.strip() for l in lines if l.strip() and not l.startswith("#")]
        self.assertEqual(sorted(sec), ["B1", "B2", "B4", "B5", "B6"])
        self.assertEqual(sec["B1"], ["bind 10.0.255.1:8080", "source 10.0.255.1", "default-server check inter 3s fall 3 rise 2",
                                     "server yyc-master1 10.0.0.3:30880", "server yyc-pool-large-worker1 10.0.0.10:30880"])
        self.assertEqual(sec["B4"], ["bind 10.0.255.2:8083", "source 10.0.255.2", "default-server check inter 3s fall 3 rise 2",
                                     "server yyc-talos-cp1 10.0.0.21:30893", "server yyc-talos-w1 10.0.0.31:30893"])
        self.assertIn("bind 10.0.255.1:8081", sec["B5"]); self.assertIn("server yyc-master1 10.0.0.3:30881", sec["B5"])
        self.assertIn("bind 10.0.255.1:8082", sec["B6"]); self.assertIn("bind 10.0.255.2:8080", sec["B2"])
        self.assertNotIn("yyc-nat1", out)
        for line in ("mode tcp", "balance roundrobin", "timeout connect 2s", "timeout check 2s",
                     "stats socket /run/haproxy/admin.sock mode 600 level admin"):
            self.assertIn(line, out)
        if HAPROXY:
            cfg = os.path.join(self.dir, "real.cfg"); write(cfg, out)
            r = subprocess.run([HAPROXY, "-c", "-q", "-f", cfg], capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


@unittest.skipUnless(HAPROXY, "haproxy is not installed (apt-get install haproxy)")
class Live(Base):
    """a test bridges.json on loopback addresses, with a real HAProxy"""

    def setUp(self):
        super().setUp()
        with open(DEF) as f:
            d = json.load(f)
        d["healthCheck"] = {"interval": 1, "timeout": 1, "retries": 1}
        d["bridges"][0]["ip"], d["bridges"][1]["ip"] = "127.0.0.2", "127.0.0.3"
        self.ports = {}
        for b in d["bridges"]:
            for s in b["services"]:
                s["listen"] = free_port([b["ip"]])
                s["nodePort"] = free_port([f"127.0.0.{i}" for i in (11, 12, 13, 14, 21, 22, 23)])
                self.ports[s["link"]] = (b["ip"], s["listen"], s["nodePort"])
        self.defpath = os.path.join(self.dir, "bridges.json"); write(self.defpath, json.dumps(d))
        self.env["BRIDGES_DEF"] = self.defpath
        f = self.fake
        f.add(1, "yyc-master1", {"yyc-cluster": "k3s", "role": "master"}, "127.0.0.11")
        f.add(2, "yyc-pool-large-worker1", {"yyc-cluster": "k3s"}, "127.0.0.12")
        f.add(3, "yyc-pool-large-worker2", {"yyc-cluster": "k3s"}, "127.0.0.13")
        f.add(4, "yyc-pool-apps-worker2", {"yyc-cluster": "k3s"}, "127.0.0.14")
        f.add(11, "yyc-talos-w1", {"yyc-cluster": "talos"}, "127.0.0.21", no_v6=True)
        f.add(12, "yyc-talos-w2", {"yyc-cluster": "talos"}, "127.0.0.22", no_v6=True)
        f.add(13, "yyc-nat1", {"yyc-cluster": "talos", "role": "nat"}, "127.0.0.30")
        self.pods(old={"jarvis": [self.p("ui-1", "app=ui", "yyc-pool-large-worker1"), self.p("ui-2", "app=ui", "yyc-pool-large-worker2")],
                       "yyc": [self.p("mcp", "app=yyc-mcp", "yyc-pool-apps-worker2"), self.p("web", "app=web", "yyc-pool-large-worker1")]},
                  new={"yyc-ai": [self.p("pigeon-1", "app=pigeon", "yyc-talos-w1"), self.p("pigeon-2", "app=pigeon", "yyc-talos-w2"),
                                  self.p("hermit-1", "app=hermit", "yyc-talos-w2")]})
        # a NodePort answers only where a callee pod runs (externalTrafficPolicy Local)
        self.listeners = {}
        for link, node in (("B1", "yyc-pool-large-worker1"), ("B1", "yyc-pool-large-worker2"), ("B5", "yyc-pool-apps-worker2"),
                           ("B6", "yyc-pool-large-worker1"), ("B2", "yyc-talos-w1"), ("B2", "yyc-talos-w2"), ("B4", "yyc-talos-w2")):
            self.listen(link, node)

    def listen(self, link, node):
        ip = self.fake.by_name(node)["private_net"][0]["ip"]
        n = Node(node, ip, self.ports[link][2]); self.nodes.append(n); self.listeners[(link, node)] = n
        return n

    def check_until(self, want_code, *a, timeout=15):
        end = time.time() + timeout
        while True:
            code, out = self.run_sh("check", *a)
            if code == want_code or time.time() > end:
                return code, out
            time.sleep(0.5)

    def box_server(self):
        return self.fake.by_name("yyc-bridge1")

    def test_up_check_down(self):
        code, out = self.run_sh("up")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.fake.violations, [], out)
        b = self.fake.created[0]
        self.assertEqual((b["name"], b["server_type"], b["location"], b["image"], b["labels"], b["ssh_keys"]),
                         ("yyc-bridge1", "cax11", "fsn1", "debian-13", {"yyc-cluster": "bridge"}, ["yyc-cluster-admin"]))
        self.assertIn("ip route replace default via 10.0.0.1", b["user_data"])
        self.assertIn("nameserver 10.0.0.30", b["user_data"])
        self.assertEqual(self.box_server()["private_net"][0]["alias_ips"], ["127.0.0.2", "127.0.0.3"])
        self.assertEqual(read(os.path.join(self.box, "etc/yyc-bridge/addresses")).split(), ["127.0.0.2", "127.0.0.3"])

        code, out = self.check_until(0)
        self.assertEqual(code, 0, out)
        self.assertIn("bridges: green", out)
        self.assertIn("ok  yyc-bridge-new B4", out)

        # traffic: round robin over the healthy callee nodes, from the bridge's own address
        ip, port, _ = self.ports["B1"]
        got = {ask(ip, port) for _ in range(6)}
        self.assertEqual(got, {"yyc-pool-large-worker1", "yyc-pool-large-worker2"})
        self.assertEqual(self.listeners[("B1", "yyc-pool-large-worker1")].peers, {"127.0.0.2"})
        ip, port, _ = self.ports["B4"]
        self.assertEqual(ask(ip, port), "yyc-talos-w2")
        self.assertEqual(self.listeners[("B4", "yyc-talos-w2")].peers, {"127.0.0.3"})

        # idempotent: a second up takes no API action and does not reload HAProxy
        n, log = len(self.fake.log), read(os.path.join(self.box, "systemctl.log"))
        code, out = self.run_sh("up")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.fake.log[n:], [], out)
        self.assertEqual(read(os.path.join(self.box, "systemctl.log")).count("reload"), log.count("reload"), out)

        # a node that runs a callee pod but has lost its Hetzner label: red until the label is back
        w2 = self.fake.by_name("yyc-pool-large-worker2")
        w2["labels"] = {"cluster": "yyc"}
        code, out = self.run_sh("check", "B1")
        self.assertEqual(code, 1, out)
        self.assertIn("RED yyc-bridge-old B1: HAProxy's targets differ from the Hetzner API's", out)
        w2["labels"] = {"yyc-cluster": "k3s"}
        self.assertEqual(self.run_sh("check", "B1")[0], 0)

        # a new server with a callee pod: no target until up re-reads the API
        self.fake.add(14, "yyc-talos-w3", {"yyc-cluster": "talos"}, "127.0.0.23", no_v6=True)
        self.pods(new={"yyc-ai": [self.p("pigeon-1", "app=pigeon", "yyc-talos-w1"), self.p("pigeon-2", "app=pigeon", "yyc-talos-w2"),
                                  self.p("hermit-1", "app=hermit", "yyc-talos-w2"), self.p("hermit-2", "app=hermit", "yyc-talos-w3")]})
        self.listen("B4", "yyc-talos-w3")
        code, out = self.run_sh("check", "B4")
        self.assertEqual(code, 1, out)
        self.assertIn("RED yyc-bridge-new B4: yyc-talos-w3 runs a callee pod but is no target", out)
        code, out = self.run_sh("up", "yyc-bridge-new")
        self.assertEqual(code, 0, out)
        code, out = self.check_until(0, "B4")
        self.assertEqual(code, 0, out)
        self.assertIn("healthy on yyc-talos-w2,yyc-talos-w3", out)

        # a node that runs a callee pod but whose NodePort fails: DOWN, red; one without a callee pod: fine (Local)
        self.listeners[("B4", "yyc-talos-w3")].close()
        code, out = self.check_until(1, "yyc-bridge-new")
        self.assertEqual(code, 1, out)
        self.assertIn("RED yyc-bridge-new B4: yyc-talos-w3 runs a callee pod but is DOWN", out)
        self.assertNotIn("B2", [l for l in out.splitlines() if l.startswith("RED")].__str__())

        # no ready callee pod at all: red
        self.pods(new={"yyc-ai": [self.p("pigeon-1", "app=pigeon", "yyc-talos-w1", ready=False)]})
        code, out = self.run_sh("check", "B2")
        self.assertEqual(code, 1, out)
        self.assertIn("no ready callee pod", out)

        # down one link: its service goes, the rest stays
        code, out = self.run_sh("down", "B5")
        self.assertEqual(code, 0, out)
        ip, port, _ = self.ports["B5"]
        with self.assertRaises(OSError):
            ask(ip, port)
        st = json.loads(read(os.path.join(self.box, "etc/yyc-bridge/state.json")))
        self.assertEqual({b["name"]: [s["link"] for s in b["services"]] for b in st["bridges"]},
                         {"yyc-bridge-old": ["B1", "B6"], "yyc-bridge-new": ["B2", "B4"]})
        code, out = self.run_sh("check", "B5")
        self.assertIn("RED yyc-bridge-old B5: not up", out)
        self.assertEqual(self.run_sh("check", "B1")[0], 0)

        # down a bridge: its services and its alias IP go
        code, out = self.run_sh("down", "yyc-bridge-old")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.box_server()["private_net"][0]["alias_ips"], ["127.0.0.3"])
        self.assertEqual(read(os.path.join(self.box, "etc/yyc-bridge/addresses")).split(), ["127.0.0.3"])
        ip, port, _ = self.ports["B1"]
        with self.assertRaises(OSError):
            ask(ip, port)

        # down with no argument is refused
        code, out = self.run_sh("down")
        self.assertNotEqual(code, 0, out)
        self.assertIsNotNone(self.box_server())

        # the last link's down deletes the server
        self.assertEqual(self.run_sh("down", "B2")[0], 0)
        self.assertIsNotNone(self.box_server())
        code, out = self.run_sh("down", "B4")
        self.assertEqual(code, 0, out)
        self.assertIsNone(self.box_server(), out)
        code, out = self.run_sh("check")
        self.assertEqual(code, 1, out)
        self.assertIn("RED yyc-bridge-old: yyc-bridge1 does not exist", out)

    def test_up_one_link_then_all(self):
        code, out = self.run_sh("up", "B4")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.box_server()["private_net"][0]["alias_ips"], ["127.0.0.3"])
        code, out = self.check_until(0, "B4")
        self.assertEqual(code, 0, out)
        code, out = self.run_sh("check", "B2")
        self.assertIn("RED yyc-bridge-new B2: not up", out)
        code, out = self.run_sh("up")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.box_server()["private_net"][0]["alias_ips"], ["127.0.0.2", "127.0.0.3"])
        self.assertEqual(self.check_until(0)[0], 0)
        code, out = self.run_sh("down", "all")
        self.assertEqual(code, 0, out)
        self.assertIsNone(self.box_server())

    def test_no_secret_in_argv_or_output(self):
        code, out = self.run_sh("up")
        self.assertEqual(code, 0, out)
        self.assertNotIn("test-token", out)
        self.assertNotIn("test-token", read(os.path.join(self.box, "systemctl.log")))


if __name__ == "__main__":
    unittest.main(verbosity=1)
