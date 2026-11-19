#!/usr/bin/env python3
"""YYC AI's own checks of the new stack's render (yyc-ai.yml job check; yyc/refactor spec-platform 13.3 items 2-4,
6, 8, 13 and 25.5, WP-31). Stdlib only. Reads the render as JSON (`kustomize build F | yq ea -o=json '[.]'`), the
folder's files and infra/secrets.txt; network checks (item 4's registry and release lookups) only with --online.

  check_deploy.py --folder yyc/deploy --render render.json [--online]

Each failure prints one line `<item> <object>: <why>`; exit 1 on any. What it asserts:
  2  no switches: pigeon and magpie take env only from Secrets and the downward API, no envFrom, no ConfigMap volume,
     args exactly [serve]; every env of every yyc-ai workload is a secretKeyRef whose Secret and key are a line of
     infra/secrets.txt, or a fieldRef (no literal value)
  B  branding: deploy/branding/ holds exactly the app's branding files, ConfigMap pigeon-branding carries them all and
     pigeon's initContainer `branding` copies it into the bundle's /branding/ after the bundle (the one ConfigMap
     volume item 2 lets pigeon have; never mounted into the pigeon container); the manifest's theme_color is white
     (the installed app's light status bar)
  3  zero downtime: every yyc-ai Deployment maxUnavailable 0, maxSurge 1, a readiness probe, a preStop (cloudflared:
     none, 9.3), its grace period at least 9.3's, a PDB minAvailable 1, replicas >= 2 or an HPA minReplicas >= 2 (and
     then no replicas), the hostname spread DoNotSchedule
  4  pins: pigeon, magpie, hermit, sandboxd one aviary sha12; hermit.json fabric.sandboxd = aviary-<that sha12>; the
     sandbox image pinned by digest; --online: each pinned image and the digest exist (anonymous `crane digest`), the
     sandbox image's dev.de0ch.aviary.speaks in the pinned magpie's and hermit's accepts, the aviary release exists
  6  every NetworkPolicy's and CiliumNetworkPolicy's app label and every Service selector match a workload
  8  egress.json: routes model and tools, the rest sandbox routes (platform 3.11.8, WP-61) each with allowPaths and
     named by a managed skill as http://<name>/; tools routes only /v1/ (decision 15: REST, no /mcp); selfHosts holds
     hermit.json's egressVip; every ${VAR} in an inject is a key of Secret yyc-ai/egress; a route to a host outside
     YYC has identity: false, and a github.com one only /DE0CH/ paths and a Basic credential from the Secret
  K  the managed CLAUDE.md, skills and title instructions fit hermit's bound (237,568 bytes as JSON);
     managed skills (decision 15, WP-62): hermit.json prompt.skills is where hermit mounts ConfigMap hermit-skills;
     its items map each key <skill>.<file> to <skill>/<file>, and they, the ConfigMap's data and hermit/skills/ are the
     same files with the same bytes; each skill has a SKILL.md; no old-stack address (JARVIS_*, --noproxy)
  13 one cluster: nothing in jarvis, kube-system or flux-system; no Namespace, no ClusterRole(Binding)
  P  placement (25.5, DESIGN 15.7): every workload requires nodes in network yyc; egress requires fsn1, the rest
     prefer it; pigeon and hermit preStop 12 s (bridge callees, R6)
  S  sizes (WP-27's review): pigeon's container ephemeral-storage limit >= its app emptyDir sizeLimit + 128Mi, its
     initContainer's memory limit >= the sizeLimit
  D  disk: no host folder without a reason (HOST_FOLDERS): it has no size and fills the node's disk (the gateway's
     image store did, 2026-11-16); scratch space listed in FENCED is an emptyDir with a sizeLimit under its
     container's ephemeral-storage limit
  G  the gateway's host.json has every field sandboxd requires (hermit 14.2); its podCidr and egressVip equal hermit.json's
     fabric; hermit/gateway.release, the gateway server's own pin, names an aviary release (--online: with sandboxd)
  H  hosts (hermit.json): a host of fabric.aws.fallbackHost takes N sandboxes (memory less the reserve over
     sandbox.memory, or CPUs x cpuOvercommit over sandbox.cpu, the smaller); fabric.aws.maxHosts x N holds
     sandbox.maxActive, and the host's disk fabric.aws.diskGiB (aviary infra.sh's 40 when absent) holds N scratch
     filesystems of sandbox.tmpGiB in full beside 16 GiB for the system and the image
     sandbox.maxActiveByPerson (a person's own number of sandboxes): none above sandbox.maxActive, and the test
     account bot@deyaochen.com (tests/e2e, the operator's checks) holds at most 3
  W  magpie-pg (news-stream/deploy/clusters/talos/db/magpie-pg.yaml) keeps min_wal_size >= 1GB and max_wal_size >= 2GB
  L  the balancer (a Service of type LoadBalancer): each port's targetPort is a name, and exactly one of the
     workloads its selector takes declares a container port of that name: the ports of one balancer lead to
     different workloads (the proxy to egress, the hosts' database port to pg-forward), never to both or to none
  F  the drive (YYC Drive): hermit.json sandbox.drive is the folder of the JuiceFS volume Deployment drive serves (the
     subPath mounted at its --root), so every sandbox's /drive is the folder staff see; a managed skill or the
     sandbox rules name /drive; the edge (drive/edge.json, the Worker's settings) names the drive
     as the server's --name does
  A  addresses: the access catalogue is people's mail addresses, so it is a Secret sealed from the store
     (infra/secrets.txt, YYC_CATALOGUE_JSON), never a file or a ConfigMap of this folder: a workload's volume
     `catalogue` is a Secret, no ConfigMap of the render and no .json file of the folder is a catalogue (JSON with a
     list `users`)
"""
import argparse, json, os, re, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
# YYC's cluster and its Secrets list are DE0CH/yyc-code's (infra/secrets.txt, news-stream/deploy/clusters/talos): a
# checkout of it, YYC_CODE (yyc.yml and deploy.yml check it out), default ./yyc-code beside this repository's root
YYC_CODE = os.environ.get("YYC_CODE", os.path.join(ROOT, "yyc-code"))
GRACE = {"pigeon": 320, "magpie": 75, "hermit": 90, "egress": 625, "tools": 180, "herald": 330, "drive": 120, "cloudflared": 200,
         "pg-forward": 60}
NO_PRESTOP = {"cloudflared"}
BRIDGE_CALLEES = {"pigeon": 12, "hermit": 12}
GATEWAY_FIELDS = ["name", "wgAddress", "hostCidr", "controlCidr", "sandboxCidr", "egressVip", "privateGateway", "podCidr"]
TEST_ACCOUNT, TEST_ACCOUNT_MAX = "bot@deyaochen.com", 3
FAIL = []


def fail(item, what, why):
    FAIL.append(f"{item} {what}: {why}")


def qty(v):
    """a Kubernetes quantity in bytes (the forms used here)"""
    m = re.fullmatch(r"(\d+(?:\.\d+)?)(Ki|Mi|Gi|Ti|k|M|G)?", str(v))
    if not m:
        raise ValueError(f"quantity {v!r}")
    n = float(m.group(1))
    return int(n * {None: 1, "Ki": 1 << 10, "Mi": 1 << 20, "Gi": 1 << 30, "Ti": 1 << 40, "k": 1e3, "M": 1e6, "G": 1e9}[m.group(2)])


def secrets_map():
    out = {}
    for ln in open(os.path.join(YYC_CODE, "infra", "secrets.txt")):
        ln = ln.strip()
        if not ln or ln.startswith("#"):
            continue
        name, _typ, _file, *pairs = ln.split()
        out.setdefault(name.split("@")[0], set()).update(p.split("=", 1)[0] for p in pairs)
    return out


def pod_spec(o):
    k = o["kind"]
    if k in ("Deployment", "DaemonSet", "StatefulSet", "Job"):
        return o["spec"]["template"]["spec"]
    if k == "CronJob":
        return o["spec"]["jobTemplate"]["spec"]["template"]["spec"]
    return None


def pod_labels(o):
    if o["kind"] == "CronJob":
        return o["spec"]["jobTemplate"]["spec"]["template"].get("metadata", {}).get("labels", {})
    return o["spec"]["template"].get("metadata", {}).get("labels", {})


def sid(o):
    return f'{o["kind"]}/{o["metadata"].get("namespace", "")}/{o["metadata"]["name"]}'


def required_terms(ps):
    na = (ps.get("affinity") or {}).get("nodeAffinity") or {}
    terms = (na.get("requiredDuringSchedulingIgnoredDuringExecution") or {}).get("nodeSelectorTerms") or []
    return terms, na.get("preferredDuringSchedulingIgnoredDuringExecution") or []


def requires(ps, key, value):
    if (ps.get("nodeSelector") or {}).get(key) == value:
        return True
    terms, _ = required_terms(ps)
    return bool(terms) and all(any(e["key"] == key and e["operator"] == "In" and e["values"] == [value]
                                   for e in t.get("matchExpressions", [])) for t in terms)


def prefers(ps, key, value):
    _, pref = required_terms(ps)
    return any(any(e["key"] == key and value in e.get("values", []) for e in p["preference"].get("matchExpressions", []))
               for p in pref)


def check_env(o, secrets):
    ps = pod_spec(o)
    for c in ps.get("containers", []) + ps.get("initContainers", []):
        if c.get("envFrom"):
            fail(2, sid(o), f"container {c['name']} has envFrom")
        for e in c.get("env", []):
            n = e["name"]
            if not re.fullmatch(r"[A-Z0-9_]+", n):
                fail(2, sid(o), f"env name {n}")
            vf = e.get("valueFrom") or {}
            if "value" in e:
                fail(2, sid(o), f"literal env {n} (bindings go through Secrets)")
            elif "secretKeyRef" in vf:
                r = vf["secretKeyRef"]
                if r["key"] not in secrets.get(f'{o["metadata"]["namespace"]}/{r["name"]}', set()):
                    fail(2, sid(o), f"env {n}: Secret {r['name']} key {r['key']} is not in infra/secrets.txt")
            elif "fieldRef" not in vf:
                fail(2, sid(o), f"env {n} is neither a secretKeyRef nor a fieldRef")


def check_serve(o):
    ps = pod_spec(o)
    for c in ps["containers"]:
        if c["name"] in ("pigeon", "magpie"):
            if c.get("args") != ["serve"]:
                fail(2, sid(o), f"{c['name']} args {c.get('args')} (exactly [serve])")
            if c.get("command"):
                fail(2, sid(o), f"{c['name']} sets command")
    # (pigeon's branding is no switch: files of the app bundle, item B)
    cms = [v for v in ps.get("volumes", []) if "projected" in v or ("configMap" in v and not (
        o["metadata"]["name"] == "pigeon" and v["configMap"].get("name", "").startswith("pigeon-branding")))]
    if cms:
        fail(2, sid(o), f"ConfigMap volume {cms[0]['name']} (nothing is mounted into pigeon or magpie but pigeon's branding)")


def check_shape(o, objs):
    name, ns = o["metadata"]["name"], o["metadata"]["namespace"]
    spec, ps = o["spec"], pod_spec(o)
    ru = (spec.get("strategy") or {}).get("rollingUpdate") or {}
    if spec.get("strategy", {}).get("type", "RollingUpdate") != "RollingUpdate" or ru.get("maxUnavailable") != 0 or ru.get("maxSurge") != 1:
        fail(3, sid(o), "strategy is not RollingUpdate maxSurge 1, maxUnavailable 0")
    for c in ps["containers"]:
        if not c.get("readinessProbe"):
            fail(3, sid(o), f"container {c['name']} has no readinessProbe")
        if name not in NO_PRESTOP and not ((c.get("lifecycle") or {}).get("preStop")):
            fail(3, sid(o), f"container {c['name']} has no preStop")
    want = GRACE.get(name)
    if want is None:
        fail(3, sid(o), "not in spec-platform 9.3's table (add it to GRACE with its drain bound)")
    elif ps.get("terminationGracePeriodSeconds", 30) < want:
        fail(3, sid(o), f"terminationGracePeriodSeconds {ps.get('terminationGracePeriodSeconds')} < {want}")
    sel = spec["selector"]["matchLabels"]
    lbl = pod_labels(o)
    pdbs = [p for p in objs if p["kind"] == "PodDisruptionBudget" and p["metadata"].get("namespace") == ns
            and all(lbl.get(k) == v for k, v in p["spec"]["selector"].get("matchLabels", {}).items())]
    if not any(p["spec"].get("minAvailable") == 1 for p in pdbs):
        fail(3, sid(o), "no PDB with minAvailable 1 selects it")
    hpas = [h for h in objs if h["kind"] == "HorizontalPodAutoscaler" and h["metadata"].get("namespace") == ns
            and h["spec"]["scaleTargetRef"]["name"] == name]
    if hpas:
        if "replicas" in spec:
            fail(3, sid(o), "has replicas beside an HPA (the HPA owns it)")
        if hpas[0]["spec"].get("minReplicas", 1) < 2:
            fail(3, sid(o), "HPA minReplicas < 2")
    elif spec.get("replicas", 1) < 2:
        fail(3, sid(o), "replicas < 2 and no HPA")
    spread = [t for t in ps.get("topologySpreadConstraints", []) if t["topologyKey"] == "kubernetes.io/hostname"]
    if not any(t.get("whenUnsatisfiable") == "DoNotSchedule" and t.get("maxSkew") == 1
               and all(t.get("labelSelector", {}).get("matchLabels", {}).get(k) == v for k, v in sel.items()) for t in spread):
        fail(3, sid(o), "no hostname spread maxSkew 1 DoNotSchedule over its own pods")
    if name in BRIDGE_CALLEES:
        for c in ps["containers"]:
            s = ((c.get("lifecycle") or {}).get("preStop") or {}).get("sleep", {}).get("seconds")
            if s != BRIDGE_CALLEES[name]:
                fail("P", sid(o), f"preStop sleep {s}, not {BRIDGE_CALLEES[name]} s (a bridge callee, DESIGN 15.5 R6)")


def check_placement(o):
    ps = pod_spec(o)
    name = o["metadata"]["name"]
    if not requires(ps, "yyc.hetzner/network", "yyc"):
        fail("P", sid(o), "does not require nodes in network yyc (yyc.hetzner/network=yyc)")
    if o["kind"] == "DaemonSet":
        return
    if name == "egress":
        # the sandboxes' exit is yyc-nat1's WARP in fsn1 wherever egress runs (DESIGN 15.7, D36, 2026-11-05): a
        # region requirement would leave it no node (YYC AI's workers are in nbg1)
        if requires(ps, "topology.kubernetes.io/region", "fsn1"):
            fail("P", sid(o), "egress must not require fsn1 (its exit is yyc-nat1, DESIGN D36)")
    elif not prefers(ps, "topology.kubernetes.io/region", "fsn1"):
        fail("P", sid(o), "does not prefer fsn1")


def check_pigeon_sizes(o):
    ps = pod_spec(o)
    app = next((v for v in ps.get("volumes", []) if v["name"] == "app"), None)
    if not app or "emptyDir" not in app:
        fail("S", sid(o), "no app emptyDir"); return
    size = qty(app["emptyDir"].get("sizeLimit", "0"))
    if app["emptyDir"].get("medium") == "Memory":
        return
    c = next(c for c in ps["containers"] if c["name"] == "pigeon")
    lim = qty(c["resources"]["limits"].get("ephemeral-storage", "0"))
    if lim < size + qty("128Mi"):
        fail("S", sid(o), f"pigeon's ephemeral-storage limit {lim >> 20}Mi < the app emptyDir {size >> 20}Mi + 128Mi "
                          "(kubelet counts a disk emptyDir against it and evicts the pod)")
    for i in ps.get("initContainers", []):
        if qty(i["resources"]["limits"].get("memory", "0")) < size:
            fail("S", sid(o), f"initContainer {i['name']}'s memory limit < the app emptyDir {size >> 20}Mi "
                              "(the copy's dirty page cache is charged to it)")


def check_selectors(objs):
    workloads = [(o["metadata"].get("namespace"), pod_labels(o)) for o in objs if pod_spec(o) is not None]

    def matches(ns, sel):
        return any(wns == ns and all(lbl.get(k) == v for k, v in sel.items()) for wns, lbl in workloads)

    for o in objs:
        ns = o["metadata"].get("namespace")
        if o["kind"] == "NetworkPolicy":
            ml = o["spec"]["podSelector"].get("matchLabels", {})
            me = o["spec"]["podSelector"].get("matchExpressions", [])
            if ml and not matches(ns, ml):
                fail(6, sid(o), f"podSelector {ml} matches no workload")
            for e in me:
                for v in e.get("values", []):
                    if not matches(ns, {e["key"]: v}):
                        fail(6, sid(o), f"podSelector {e['key']}={v} matches no workload")
            for rule in o["spec"].get("ingress", []) + o["spec"].get("egress", []):
                for peer in rule.get("from", []) + rule.get("to", []):
                    if "namespaceSelector" in peer or "ipBlock" in peer:
                        continue
                    pl = (peer.get("podSelector") or {}).get("matchLabels", {})
                    if pl and not matches(ns, pl):
                        fail(6, sid(o), f"peer {pl} matches no workload")
        elif o["kind"] == "CiliumNetworkPolicy":
            ml = o["spec"]["endpointSelector"].get("matchLabels", {})
            if not matches(ns, ml):
                fail(6, sid(o), f"endpointSelector {ml} matches no workload")
        elif o["kind"] == "Service" and o["spec"].get("selector"):
            if not matches(ns, o["spec"]["selector"]):
                fail(6, sid(o), f"selector {o['spec']['selector']} matches no workload")


def check_balancer(objs):
    for svc in objs:
        if svc["kind"] != "Service" or svc["spec"].get("type") != "LoadBalancer":
            continue
        ns, sel = svc["metadata"].get("namespace"), svc["spec"].get("selector") or {}
        taken = [o for o in objs if pod_spec(o) is not None and o["metadata"].get("namespace") == ns
                 and all(pod_labels(o).get(k) == v for k, v in sel.items())]
        for port in svc["spec"].get("ports", []):
            tp = port.get("targetPort")
            if not isinstance(tp, str):
                fail("L", sid(svc), f"port {port.get('port')}: targetPort {tp!r} is not a port name")
                continue
            have = [o["metadata"]["name"] for o in taken
                    if any(cp.get("name") == tp for c in pod_spec(o)["containers"] for cp in c.get("ports", []))]
            if len(have) != 1:
                fail("L", sid(svc), f"port {port.get('port')} ({tp}): declared by {have or 'no workload'} "
                                    "of the selected workloads (exactly one)")


def clean_prefix(p):
    """egress's own rule for a sandbox route's path prefix (jarvis3 chat/src/egress/config.ts cleanPrefix)."""
    if not isinstance(p, str) or not p.startswith("/") or not p.endswith("/") or "//" in p:
        return False
    if re.search(r"%2e|%2f|%5c|\\", p, re.I):
        return False
    return all(s not in (".", "..") for s in p[1:-1].split("/"))


def check_egress(folder, secrets):
    e = json.load(open(os.path.join(folder, "egress", "egress.json")))
    h = json.load(open(os.path.join(folder, "hermit", "hermit.json")))
    names = sorted(r["name"] for r in e.get("routes", []))
    if not {"model", "tools"} <= set(names):
        fail(8, "egress.json", f"routes {names}, without model and tools")
    skills_text = "".join(skill_files(folder).values())
    for r in e.get("routes", []):
        if r["name"] == "tools" and r.get("allowPaths") != ["/v1/"]:
            fail(8, "egress.json", f"route tools allowPaths {r.get('allowPaths')}, not ['/v1/'] (decision 15)")
        if r["name"] in ("model", "tools"):
            continue
        if not r.get("allowPaths"):
            fail(8, "egress.json", f"sandbox route {r['name']} has no allowPaths")
        for path in r.get("allowPaths") or []:
            if not clean_prefix(path):
                fail(8, "egress.json", f"sandbox route {r['name']} allowPaths {path!r} is not /…/ (egress refuses to "
                     "start: jarvis3 chat/src/egress/config.ts cleanPrefix)")
        # an upstream outside YYC (not an in-cluster name, not under yyc-internal.com) gets no staff identity, and a
        # GitHub one is one of DE0CH's repositories with a credential from the Secret (the token is read-only)
        host = re.sub(r"^https?://([^/:]+).*$", r"\1", r.get("upstream", ""))
        if "." in host and not host.endswith(".yyc-internal.com") and not host.endswith(".svc") \
                and not host.endswith(".cluster.local") and r.get("identity") is not False:
            fail(8, "egress.json", f"sandbox route {r['name']} goes to {host}, outside YYC, without identity: false "
                 "(the Aviary-* headers would carry the staff member's e-mail there)")
        if host == "github.com":
            if not all(p.startswith("/DE0CH/") for p in r.get("allowPaths") or []):
                fail(8, "egress.json", f"sandbox route {r['name']}: a github.com route allows only /DE0CH/<repo>/ paths")
            if not re.fullmatch(r"Basic \$\{[A-Z0-9_]+\}", r.get("inject", {}).get("Authorization", "")):
                fail(8, "egress.json", f"sandbox route {r['name']}: a github.com route injects Authorization: Basic "
                     "${VAR} (a read-only token of Secret yyc-ai/egress)")
        if f"http://{r['name']}/" not in skills_text:
            fail(8, "egress.json", f"sandbox route {r['name']}: no managed skill names http://{r['name']}/")
    if h["fabric"]["egressVip"] not in e.get("selfHosts", []):
        fail(8, "egress.json", f"selfHosts lacks hermit.json's egressVip {h['fabric']['egressVip']}")
    keys = secrets.get("yyc-ai/egress", set())
    for r in e.get("routes", []):
        for v in r.get("inject", {}).values():
            for var in re.findall(r"\$\{([A-Z0-9_]+)\}", v):
                if var not in keys:
                    fail(8, "egress.json", f"route {r['name']} injects ${{{var}}}, not a key of Secret yyc-ai/egress")
    return h


# web_fetch reads a page itself when the capture and Exa cannot (jarvis3 chat/src/tools/netguard.ts safeGet): http
# and https on these ports, public addresses only
WEB_FETCH_PORTS = {80, 443, 8080, 8443}


def check_tools_out(objs):
    """tools reaches the public internet on every port web_fetch's own reader uses (an http:// page is port 80)."""
    p = next((o for o in objs if o["kind"] == "NetworkPolicy" and o["metadata"]["name"] == "tools-out"), None)
    if p is None:
        return fail(6, "NetworkPolicy/tools-out", "missing")
    open_ports = set()
    for rule in p["spec"].get("egress", []):
        if any((t.get("ipBlock") or {}).get("cidr") == "0.0.0.0/0" for t in rule.get("to", [])):
            open_ports |= {x.get("port") for x in rule.get("ports", []) if x.get("protocol", "TCP") == "TCP"}
    missing = sorted(WEB_FETCH_PORTS - open_ports)
    if missing:
        fail(6, "NetworkPolicy/tools-out", f"the internet rule lacks port(s) {missing}: web_fetch cannot read a page "
             "there (http:// pages are port 80)")


def skill_files(folder):
    """hermit/skills/<skill>/<file> -> text"""
    base = os.path.join(folder, "hermit", "skills")
    out = {}
    for skill in sorted(os.listdir(base)) if os.path.isdir(base) else []:
        for f in sorted(os.listdir(os.path.join(base, skill))):
            out[f"{skill}/{f}"] = open(os.path.join(base, skill, f), encoding="utf-8").read()
    return out


MANAGED_MAX_BYTES = 237568


def check_skills(folder, objs, h):
    files = skill_files(folder)
    if not files:
        fail("K", "hermit/skills", "no managed skills")
    for k, text in files.items():
        if re.search(r"JARVIS_|--noproxy", text):
            fail("K", f"hermit/skills/{k}", "an old-stack address (JARVIS_*, --noproxy): the new stack calls http://<name>/ through the proxy")
    for skill in {k.split("/")[0] for k in files}:
        if f"{skill}/SKILL.md" not in files:
            fail("K", f"hermit/skills/{skill}", "no SKILL.md")
    cm = next((o for o in objs if o["kind"] == "ConfigMap" and o["metadata"]["name"].startswith("hermit-skills")), None)
    dep = next((o for o in objs if o["kind"] == "Deployment" and o["metadata"]["name"] == "hermit"), None)
    if cm is None or dep is None:
        fail("K", "hermit-skills", "no ConfigMap hermit-skills or no Deployment hermit")
        return
    data = cm.get("data", {})
    want = {k.replace("/", ".", 1): v for k, v in files.items()}
    if data != want:
        fail("K", sid(cm), f"data {sorted(data)} differs from hermit/skills/ {sorted(want)} (keys or bytes)")
    # hermit refuses a hermit.json whose managed CLAUDE.md, skills and title instructions are over 237,568 bytes as
    # JSON (hermit-api MANAGED_MAX_BYTES; the new hermit Pod then never turns ready and the rollout fails after
    # 15 min, 2026-11-13). The same sum, from the files and the ConfigMap's skills
    base = os.path.join(folder, "hermit")
    read = lambda name: open(os.path.join(base, name), encoding="utf-8").read()
    md = read("system-prompt.md").rstrip("\n") + "\n\n" + read("sandbox-rules.md")
    managed = len(json.dumps([md, {k.replace(".", "/", 1): v for k, v in data.items()}, read("title-system.md")],
                             ensure_ascii=False, separators=(",", ":")).encode())
    if managed > MANAGED_MAX_BYTES:
        fail("K", "hermit/skills", f"the managed CLAUDE.md, skills and title instructions are {managed} bytes as JSON, over {MANAGED_MAX_BYTES}: hermit refuses them")
    spec = dep["spec"]["template"]["spec"]
    vol = next((v for v in spec.get("volumes", []) if v.get("configMap", {}).get("name") == cm["metadata"]["name"]), None)
    if vol is None:
        fail("K", sid(dep), "does not mount ConfigMap hermit-skills")
        return
    items = {i["key"]: i["path"] for i in vol["configMap"].get("items", [])}
    if items != {k.replace("/", ".", 1): k for k in files}:
        fail("K", sid(dep), f"volume {vol['name']} items {items} do not map every <skill>.<file> to <skill>/<file>")
    mounts = [m["mountPath"] for c in spec["containers"] for m in c.get("volumeMounts", []) if m["name"] == vol["name"]]
    if h["prompt"].get("skills") not in mounts:
        fail("K", "hermit.json", f"prompt.skills {h['prompt'].get('skills')!r} is not where hermit mounts the skills ({mounts})")


# The app is one build for every deployment (claudecodeui paseo/packages/app src/yyc/brand.ts BRAND_FILES): what
# names YYC to the eye is these files, which pigeon serves at /branding/ in place of the bundle's neutral ones.
BRAND_FILES = ["logo.png", "favicon.png", "favicon-running.png", "favicon-attention.png", "apple-touch-icon.png",
               "icon-192.png", "icon-512.png", "manifest.json"]


def check_branding(folder, objs):
    base = os.path.join(folder, "branding")
    have = sorted(os.listdir(base)) if os.path.isdir(base) else []
    if have != sorted(BRAND_FILES):
        fail("B", "branding/", f"files {have} are not exactly {sorted(BRAND_FILES)}")
    cm = next((o for o in objs if o["kind"] == "ConfigMap" and o["metadata"]["name"].startswith("pigeon-branding")), None)
    dep = next((o for o in objs if o["kind"] == "Deployment" and o["metadata"]["name"] == "pigeon"), None)
    if cm is None or dep is None:
        fail("B", "pigeon-branding", "no ConfigMap pigeon-branding or no Deployment pigeon")
        return
    keys = set(cm.get("data", {})) | set(cm.get("binaryData", {}))
    for f in BRAND_FILES:
        if f not in keys:
            fail("B", sid(cm), f"{f} is missing: the app would show the bundle's neutral {f}")
    # An app installed from Chrome on Android (claudecodeui docs/paseo-ui.md "Installed on a phone"): Chrome ignores a
    # page theme-color brighter than about #eeeeee and paints the status bar with the manifest's, so the manifest
    # carries the light theme's background; a dark page colour is honoured, and in the dark scheme Chrome uses its own
    try:
        theme = json.loads(cm.get("data", {}).get("manifest.json", "{}")).get("theme_color")
    except ValueError:
        theme = None
    if theme != "#ffffff":
        fail("B", sid(cm), f"manifest.json theme_color is {theme!r}, not \"#ffffff\": the installed app's status bar "
                           "would be dark over the light theme")
    spec = dep["spec"]["template"]["spec"]
    vol = next((v for v in spec.get("volumes", []) if v.get("configMap", {}).get("name") == cm["metadata"]["name"]), None)
    inits = [i["name"] for i in spec.get("initContainers", [])]
    i = next((i for i in spec.get("initContainers", []) if i["name"] == "branding"), None)
    mounts = {m["name"]: m for m in (i or {}).get("volumeMounts", [])}
    if (vol is None or i is None or i.get("command") != ["/cp", "-rL", "/branding", "/srv/app/"]
            or mounts.get(vol["name"], {}).get("mountPath") != "/branding" or mounts.get("app", {}).get("mountPath") != "/srv/app"
            or inits.index("branding") < inits.index("app")):
        fail("B", sid(dep), "no initContainer branding after app that copies ConfigMap pigeon-branding (/branding) into "
                            "the bundle (/cp -rL /branding /srv/app/)")
    # never a mount over the bundle's /branding/ in pigeon: the bundle is read-only there, and where it has no such
    # directory (an app from before it) the container cannot start
    for c in spec["containers"]:
        if vol is not None and any(m["name"] == vol["name"] for m in c.get("volumeMounts", [])):
            fail("B", sid(dep), f"container {c['name']} mounts the branding (the initContainer copies it into the bundle)")


def check_drive(folder, objs, h):
    d = next((o for o in objs if o["kind"] == "Deployment" and o["metadata"]["name"] == "drive"), None)
    if d is None:
        return
    c = next(c for c in pod_spec(d)["containers"] if c["name"] == "drive")
    args = c.get("args", [])
    root = args[args.index("--root") + 1] if "--root" in args else None
    serves = next((m.get("subPath") for m in c.get("volumeMounts", []) if m["mountPath"] == root), None)
    have = h["sandbox"].get("drive")
    if have is None:
        fail("F", "hermit.json", f"no sandbox.drive: the sandboxes lack the drive's folder {serves} at /drive")
    elif have != serves:
        fail("F", "hermit.json", f"sandbox.drive {have}, but Deployment drive serves {serves}")
    name = args[args.index("--name") + 1] if "--name" in args else None
    edge = json.load(open(os.path.join(folder, "drive", "edge.json"), encoding="utf-8"))
    if edge.get("DRIVE_NAME") != name:
        fail("F", "drive/edge.json", f"DRIVE_NAME {edge.get('DRIVE_NAME')!r}, but Deployment drive's --name is {name!r}: "
                                     "the page would name the drive twice, differently")
    texts = list(skill_files(folder).values())
    rules = os.path.join(folder, "hermit", "sandbox-rules.md")
    if os.path.exists(rules):
        texts.append(open(rules, encoding="utf-8").read())
    if have and not any(re.search(r"(?<![\w/.-])/drive\b", t) for t in texts):
        fail("F", "hermit/skills", "no managed skill and not the sandbox rules names /drive: the model never looks there")


def check_hosts(h):
    s, f = h["sandbox"], h["fabric"]
    fb = f["aws"]["fallbackHost"]
    n = min((qty(fb["memory"]) - qty(fb["reserve"])) // qty(s["memory"]), int(fb["cpus"] * f["cpuOvercommit"] / s["cpu"]))
    if n < 1:
        fail("H", "hermit.json", f"a host takes no sandbox of sandbox.memory {s['memory']}")
        return
    if f["aws"]["maxHosts"] * n < s["maxActive"]:
        fail("H", "hermit.json", f"fabric.aws.maxHosts {f['aws']['maxHosts']} x {n} sandboxes a host holds fewer than "
             f"sandbox.maxActive {s['maxActive']}")
    by = s.get("maxActiveByPerson", {})
    for who, m in by.items():
        if m > s["maxActive"]:
            fail("H", "hermit.json", f"sandbox.maxActiveByPerson {who}: {m} is above sandbox.maxActive {s['maxActive']}")
    if not 1 <= by.get(TEST_ACCOUNT, by.get("*", 0)) <= TEST_ACCOUNT_MAX:
        fail("H", "hermit.json", f"sandbox.maxActiveByPerson: the test account {TEST_ACCOUNT} holds at most "
             f"{TEST_ACCOUNT_MAX} sandboxes (its conversations once held nine of the ten)")
    disk, need = f["aws"].get("diskGiB", 40), n * s["tmpGiB"] + 16
    if disk < need:
        fail("H", "hermit.json", f"fabric.aws.diskGiB {disk} holds fewer than {n} sandboxes of sandbox.tmpGiB "
             f"{s['tmpGiB']} beside 16 GiB for the system ({need} GiB)")


def check_gateway(folder, h, online=False):
    g = json.load(open(os.path.join(folder, "hermit", "gateway-host.json")))
    for f in GATEWAY_FIELDS:
        if not g.get(f):
            fail("G", "gateway-host.json", f"no {f} (sandboxd refuses to start without it)")
    if g.get("podCidr") != h["fabric"]["podCidr"] or g.get("egressVip") != h["fabric"]["egressVip"]:
        fail("G", "gateway-host.json", "podCidr or egressVip differs from hermit.json's fabric")
    if g.get("wgPort") != 51821 or not g.get("gateway"):
        fail("G", "gateway-host.json", "not the gateway on udp 51821")
    # the gateway server's own pin (infra/gateway.sh): it follows no cluster pin, so it changes only when meant to
    rel = open(os.path.join(folder, "hermit", "gateway.release")).read().strip()
    if not re.fullmatch(r"aviary-[0-9a-f]{12}", rel):
        fail("G", "gateway.release", f"'{rel}' is not an aviary release (aviary-<sha12>)")
    elif online:
        r = subprocess.run(["curl", "-fsS", "--retry", "5", "--retry-all-errors", "-o", "/dev/null", "-w", "%{http_code}", "-I",
                            f"https://github.com/DE0CH/aviary/releases/download/{rel}/sandboxd"], capture_output=True, text=True)
        if r.stdout.strip() not in ("200", "302"):
            fail("G", "gateway.release", f"release {rel} has no sandboxd binary")
    if "2001:db8:" in h["fabric"]["gateway"]["endpoint"]:
        print(f"::warning::hermit.json fabric.gateway.endpoint is a placeholder ({h['fabric']['gateway']['endpoint']}): "
              "set the gateway server's public IPv6")


# A host folder has no size and no accounting: what a pod writes there fills the node's disk, and the kubelet answers
# with disk pressure on the whole node. Only these, each with why it cannot be a pod's own scratch space.
HOST_FOLDERS = {}
# Scratch space that must be the pod's own, with a size: (kind, name, container, mount path).
FENCED = []


def check_disk(objs):
    for o in objs:
        ps = pod_spec(o)
        if ps is None:
            continue
        for v in ps.get("volumes") or []:
            path = (v.get("hostPath") or {}).get("path")
            if path and (o["kind"], o["metadata"]["name"], path) not in HOST_FOLDERS:
                fail("D", sid(o), f"host folder {path}: no size, it fills the node's disk (an emptyDir with a "
                                  "sizeLimit, or a reason in HOST_FOLDERS)")
    for kind, name, cname, mount in FENCED:
        o = next((x for x in objs if x["kind"] == kind and x["metadata"]["name"] == name), None)
        if o is None:
            fail("D", f"{kind}/{name}", "missing")
            continue
        ps = pod_spec(o)
        c = next((c for c in ps["containers"] if c["name"] == cname), {})
        vol = next((m["name"] for m in c.get("volumeMounts") or [] if m["mountPath"] == mount), None)
        v = next((v for v in ps.get("volumes") or [] if v["name"] == vol), {})
        if "emptyDir" not in v:
            fail("D", sid(o), f"{mount} is not an emptyDir of its own (it would be the host folder under it)")
        elif not v["emptyDir"].get("sizeLimit"):
            fail("D", sid(o), f"{mount}: the emptyDir has no sizeLimit")
        lim = ((c.get("resources") or {}).get("limits") or {}).get("ephemeral-storage")
        if not lim:
            fail("D", sid(o), f"container {cname} has no ephemeral-storage limit (the kubelet evicts this pod alone "
                              "when it is over, not the node's pods under disk pressure)")
        elif v.get("emptyDir", {}).get("sizeLimit") and qty(lim) < qty(v["emptyDir"]["sizeLimit"]):
            fail("D", sid(o), f"ephemeral-storage limit {lim} is under {mount}'s sizeLimit")


# a lookup that never reached the registry (the runner's DNS or network), retried; a registry's answer is final
NETWORK_ERROR = re.compile(r"dial tcp|i/o timeout|no such host|connection (refused|reset)|TLS handshake timeout|EOF$")


def crane_config(ref, tries=6):
    """`crane config ref`, retried while the failure is the network's (2026-11-17: the runner's DNS timed out on
    ghcr.io for 90 s and every pin read as not pullable); CHECK_DEPLOY_RETRY_PAUSE seconds between tries (tests: 0)"""
    pause = float(os.environ.get("CHECK_DEPLOY_RETRY_PAUSE", "20"))
    for i in range(tries):
        r = subprocess.run(["crane", "config", ref], capture_output=True, text=True)
        if r.returncode == 0 or not NETWORK_ERROR.search(r.stderr) or i == tries - 1:
            return r
        time.sleep(pause)
    return r


def check_pins(folder, objs, online):
    k = open(os.path.join(folder, "kustomization.yaml")).read()
    tags = dict(re.findall(r"\{name: ([a-z0-9-]+), newName: [^,]+, newTag: \"?([0-9a-f]+)\"?\}", k))
    aviary = {tags.get(n) for n in ("pigeon", "magpie", "hermit", "sandboxd")}
    if len(aviary) != 1 or None in aviary:
        fail(4, "kustomization.yaml", f"pigeon, magpie, hermit, sandboxd are not one aviary sha12: {tags}")
        return
    sha = aviary.pop()
    h = json.load(open(os.path.join(folder, "hermit", "hermit.json")))
    if h["fabric"]["sandboxd"] != f"aviary-{sha}":
        fail(4, "hermit.json", f"fabric.sandboxd {h['fabric']['sandboxd']} != aviary-{sha} (the sandboxd image tag)")
    img = h["sandbox"]["image"]
    if not re.fullmatch(r"ghcr\.io/de0ch/[a-z0-9-]+@sha256:[0-9a-f]{64}", img):
        fail(4, "hermit.json", f"sandbox.image {img} is not pinned by digest")
    if not online:
        return
    labels = {}
    for n in ("pigeon", "magpie", "hermit", "sandboxd", "pigeon-app", "egress"):
        ref = f"ghcr.io/de0ch/{n}:{tags[n]}"
        r = crane_config(ref)
        if r.returncode:
            fail(4, ref, f"not pullable anonymously: {r.stderr.strip()[:200]}")
            continue
        labels[n] = (json.loads(r.stdout).get("config") or {}).get("Labels") or {}
    r = crane_config(img)
    if r.returncode:
        fail(4, img, f"not pullable anonymously: {r.stderr.strip()[:200]}")
    else:
        speaks = json.loads(((json.loads(r.stdout).get("config") or {}).get("Labels") or {}).get("dev.de0ch.aviary.speaks") or "{}")
        for n, proto in (("magpie", "magpie-sandbox"), ("hermit", "hermit-sandbox")):
            acc = json.loads(labels.get(n, {}).get("dev.de0ch.aviary.accepts") or "{}")
            if proto not in speaks:
                fail(4, img, f"speaks no {proto}")
            elif speaks[proto] not in acc.get(proto, []):
                fail(4, n, f"accepts {proto} {acc.get(proto)}, the sandbox image speaks {speaks[proto]}")
    rel = subprocess.run(["curl", "-fsS", "--retry", "5", "--retry-all-errors", "-o", "/dev/null", "-w", "%{http_code}",
                          f"https://github.com/DE0CH/aviary/releases/tag/aviary-{sha}"], capture_output=True, text=True)
    if rel.stdout.strip() != "200":
        fail(4, f"aviary-{sha}", "release not found")


def is_catalogue(text):
    """text is an access catalogue: JSON with a list `users`"""
    try:
        c = json.loads(text)
    except (ValueError, TypeError):
        return False
    return isinstance(c, dict) and isinstance(c.get("users"), list)


def check_catalogue(folder, objs):
    """A: who may use YYC's apps is a list of people's addresses: it reaches a workload as a Secret, and is neither a
    file of the folder nor a ConfigMap of the render"""
    for o in objs:
        for v in (pod_spec(o) or {}).get("volumes") or []:
            if v["name"] == "catalogue" and "secret" not in v:
                fail("A", sid(o), "volume catalogue is not a Secret (the access catalogue is sealed from the store, never in git)")
        if o["kind"] == "ConfigMap":
            for k, v in (o.get("data") or {}).items():
                if is_catalogue(v):
                    fail("A", sid(o), f"{k} is an access catalogue in a ConfigMap (people's addresses: sealed from the store, never in git)")
    for d, _, files in os.walk(folder):
        for f in files:
            p = os.path.join(d, f)
            if f.endswith(".json") and is_catalogue(open(p, encoding="utf-8", errors="replace").read()):
                fail("A", os.path.relpath(p, folder), "an access catalogue is a file of the folder (people's addresses: the store's YYC_CATALOGUE_JSON, never in git)")


def check_wal():
    p = os.path.join(YYC_CODE, "news-stream/deploy/clusters/talos/db/magpie-pg.yaml")
    if not os.path.exists(p):
        return
    s = open(p).read()
    for k, floor in (("min_wal_size", qty("1Gi")), ("max_wal_size", qty("2Gi"))):
        m = re.search(rf"^\s+{k}:\s*\"?(\d+)(MB|GB|TB)\"?", s, re.M)
        v = int(m.group(1)) * {"MB": 1 << 20, "GB": 1 << 30, "TB": 1 << 40}[m.group(2)] if m else 0
        if v < floor:
            fail("W", "magpie-pg.yaml", f"{k} missing or below {floor >> 30}GB (magpie 6.10's latency bound needs it)")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--folder", required=True)
    ap.add_argument("--render", required=True)
    ap.add_argument("--online", action="store_true")
    a = ap.parse_args()
    folder = os.path.join(ROOT, a.folder)
    objs = [o for o in json.load(open(a.render)) if o]
    secrets = secrets_map()
    for o in objs:
        ns = o["metadata"].get("namespace")
        if ns in ("jarvis", "kube-system", "flux-system"):
            fail(13, sid(o), f"in namespace {ns}: the cluster's own")
        if o["kind"] in ("Namespace", "ClusterRole", "ClusterRoleBinding"):
            fail(13, sid(o), "cluster-level (news-stream/deploy/clusters/talos/)")
    for o in objs:
        if pod_spec(o) is None:
            continue
        if o["metadata"].get("namespace") == "yyc-ai":
            check_env(o, secrets)
            if o["kind"] == "Deployment" and o["metadata"]["name"] in ("pigeon", "magpie"):
                check_serve(o)
        check_placement(o)
        if o["kind"] == "Deployment" and o["metadata"].get("namespace") == "yyc-ai":
            check_shape(o, objs)
        if o["kind"] == "Deployment" and o["metadata"]["name"] == "pigeon":
            check_pigeon_sizes(o)
    check_selectors(objs)
    check_balancer(objs)
    check_tools_out(objs)
    h = check_egress(folder, secrets)
    check_skills(folder, objs, h)
    check_gateway(folder, h, a.online)
    check_hosts(h)
    check_drive(folder, objs, h)
    check_disk(objs)
    check_branding(folder, objs)
    check_catalogue(folder, objs)
    check_pins(folder, objs, a.online)
    check_wal()
    for f in FAIL:
        print(f"::error::{f}")
    print("ok" if not FAIL else f"{len(FAIL)} problem(s)")
    sys.exit(1 if FAIL else 0)


if __name__ == "__main__":
    main()
