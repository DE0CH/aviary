#!/usr/bin/env python3
"""check_deploy.py catches seeded violations (stdlib only; yyc-ai.yml check runs it after the render): each test
mutates one object of the real render and expects that check's failure. Needs render.json from
`kustomize build yyc-ai/deploy | yq ea -o=json '[.]'` (RENDER env, default the working directory)."""
import copy, json, os, subprocess, sys, tempfile, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
FOLDER = os.environ.get("FOLDER", "yyc-ai/deploy")
RENDER = json.load(open(os.environ.get("RENDER", "render.json")))


def run(objs):
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        json.dump(objs, f)
    return subprocess.run([sys.executable, "-I", os.path.join(HERE, "check_deploy.py"), "--folder", FOLDER,
                           "--render", f.name], capture_output=True, text=True)


def find(objs, kind, name):
    return next(o for o in objs if o and o["kind"] == kind and o["metadata"]["name"] == name)


def container(o, name):
    return next(c for c in o["spec"]["template"]["spec"]["containers"] if c["name"] == name)


class Seeded(unittest.TestCase):
    def expect(self, mutate, item, text):
        objs = copy.deepcopy(RENDER)
        mutate(objs)
        r = run(objs)
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn(f"::error::{item} ", r.stdout)
        self.assertIn(text, r.stdout)

    def test_sandbox_route_path_shape(self):
        # egress refuses to start on a sandbox route path that is not /…/ (platform 3.11.8): the first herald deploy
        # crash-looped the new egress pod on "/v1/channels"
        import shutil
        with tempfile.TemporaryDirectory() as d:
            folder = os.path.join(d, "deploy")
            shutil.copytree(FOLDER, folder)
            p = os.path.join(folder, "egress", "egress.json")
            e = json.load(open(p))
            next(r for r in e["routes"] if r["name"] not in ("model", "tools"))["allowPaths"] = ["/v1/channels"]
            json.dump(e, open(p, "w"))
            with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
                json.dump(RENDER, f)
            r = subprocess.run([sys.executable, "-I", os.path.join(HERE, "check_deploy.py"), "--folder", folder,
                                "--render", f.name], capture_output=True, text=True)
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("::error::8 ", r.stdout)
        self.assertIn("/v1/channels", r.stdout)

    def egress_json(self, mutate):
        import shutil
        with tempfile.TemporaryDirectory() as d:
            folder = os.path.join(d, "deploy")
            shutil.copytree(FOLDER, folder)
            p = os.path.join(folder, "egress", "egress.json")
            e = json.load(open(p))
            mutate(e)
            json.dump(e, open(p, "w"))
            with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
                json.dump(RENDER, f)
            return subprocess.run([sys.executable, "-I", os.path.join(HERE, "check_deploy.py"), "--folder", folder,
                                   "--render", f.name], capture_output=True, text=True)

    def hermit_json(self, mutate):
        import shutil
        with tempfile.TemporaryDirectory() as d:
            folder = os.path.join(d, "deploy")
            shutil.copytree(FOLDER, folder)
            p = os.path.join(folder, "hermit", "hermit.json")
            h = json.load(open(p))
            mutate(h)
            json.dump(h, open(p, "w"))
            with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
                json.dump(RENDER, f)
            return subprocess.run([sys.executable, "-I", os.path.join(HERE, "check_deploy.py"), "--folder", folder,
                                   "--render", f.name], capture_output=True, text=True)

    def test_hosts_hold_the_sandboxes(self):
        # 7Gi sandboxes: 2 a host. 3 hosts hold 6, fewer than maxActive 10; and two 100 GiB scratch filesystems
        # do not fit a 40 GiB disk (they are sparse files: nothing fails until a sandbox's /tmp runs short)
        def big(h, hosts, disk):
            h["sandbox"].update(memory="7Gi", tmpGiB=100, maxActive=10)
            h["fabric"]["aws"].update(maxHosts=hosts, fallbackHost={"memory": "15.5Gi", "reserve": "1Gi", "cpus": 4})
            h["fabric"]["aws"].pop("diskGiB", None)
            if disk:
                h["fabric"]["aws"]["diskGiB"] = disk
        r = self.hermit_json(lambda h: big(h, 3, 220))
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("::error::H ", r.stdout)
        self.assertIn("maxHosts 3 x 2 sandboxes", r.stdout)
        for disk in (None, 215):
            r = self.hermit_json(lambda h: big(h, 5, disk))
            self.assertEqual(r.returncode, 1, r.stdout)
            self.assertIn("::error::H ", r.stdout)
            self.assertIn("(216 GiB)", r.stdout)
        r = self.hermit_json(lambda h: big(h, 5, 220))
        self.assertNotIn("::error::H ", r.stdout)

    def test_outside_route_carries_no_identity(self):
        # the route code goes to github.com: without identity: false egress would send the staff member's e-mail
        # (Aviary-Sandbox-Person) to GitHub with every git request
        r = self.egress_json(lambda e: next(x for x in e["routes"] if x["name"] == "code").pop("identity"))
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("::error::8 ", r.stdout)
        self.assertIn("without identity: false", r.stdout)

    def test_github_route_only_our_repositories_and_a_secret_credential(self):
        code = lambda e: next(x for x in e["routes"] if x["name"] == "code")
        r = self.egress_json(lambda e: code(e).update(allowPaths=["/torvalds/linux/"]))
        self.assertIn("only /DE0CH/<repo>/ paths", r.stdout)
        r = self.egress_json(lambda e: code(e).update(inject={"Authorization": "Basic eDp5"}))
        self.assertIn("injects Authorization: Basic ${VAR}", r.stdout)

    def test_clean(self):
        r = run(RENDER)
        self.assertEqual(r.returncode, 0, r.stdout)

    def test_tools_out_ports(self):
        # tools-out allowed 443 only: web_fetch's own reader timed out on every http:// page
        def only_443(objs):
            for rule in find(objs, "NetworkPolicy", "tools-out")["spec"]["egress"]:
                if any((t.get("ipBlock") or {}).get("cidr") == "0.0.0.0/0" for t in rule.get("to", [])):
                    rule["ports"] = [{"port": 443, "protocol": "TCP"}]
        self.expect(only_443, 6, "lacks port(s) [80, 8080, 8443]")

    def test_literal_env(self):
        self.expect(lambda o: container(find(o, "Deployment", "pigeon"), "pigeon")["env"].append({"name": "PIGEON_X", "value": "1"}), 2, "literal env")

    def test_the_drive_takes_no_env(self):
        # YYC Drive's settings are flags: an env on it is a literal binding like any other
        self.expect(lambda o: container(find(o, "Deployment", "drive"), "drive").update(env=[{"name": "DRIVE_ROOT", "value": "/x"}]), 2, "literal env")

    def test_the_drive_is_reached_only_by_a_workload(self):
        # its only way in is cloudflared; a policy naming a pod that does not exist is caught
        def m(objs):
            find(objs, "NetworkPolicy", "drive-in")["spec"]["podSelector"]["matchLabels"]["app"] = "drivee"
        self.expect(m, 6, "drive-in")

    def test_args(self):
        self.expect(lambda o: container(find(o, "Deployment", "magpie"), "magpie").update(args=["serve", "--x"]), 2, "exactly [serve]")

    def test_configmap_in_pigeon(self):
        self.expect(lambda o: find(o, "Deployment", "pigeon")["spec"]["template"]["spec"]["volumes"].append({"name": "c", "configMap": {"name": "x"}}), 2, "ConfigMap volume")

    # the one thing mounted into pigeon: YYC's branding over the app bundle's neutral files (item B)
    def test_branding_file_missing(self):
        def m(o):
            cm = next(x for x in o if x and x["kind"] == "ConfigMap" and x["metadata"]["name"].startswith("pigeon-branding"))
            cm.get("binaryData", {}).pop("logo.png", None)
        self.expect(m, "B", "logo.png")

    # Chrome on Android paints the installed app's light status bar from the manifest (a near-white page colour is ignored)
    def test_branding_manifest_theme_colour(self):
        def m(o):
            cm = next(x for x in o if x and x["kind"] == "ConfigMap" and x["metadata"]["name"].startswith("pigeon-branding"))
            cm["data"]["manifest.json"] = cm["data"]["manifest.json"].replace('"#ffffff"', '"#181b1a"')
        self.expect(m, "B", "theme_color")

    def test_branding_not_copied(self):
        def m(o):
            ps = find(o, "Deployment", "pigeon")["spec"]["template"]["spec"]
            ps["initContainers"] = [i for i in ps["initContainers"] if i["name"] != "branding"]
        self.expect(m, "B", "initContainer branding")

    # a mount over the bundle's /branding/ cannot start where the bundle has no such directory (2026-11-13)
    def test_branding_mounted_into_pigeon(self):
        def m(o):
            container(find(o, "Deployment", "pigeon"), "pigeon")["volumeMounts"].append(
                {"name": "branding", "mountPath": "/srv/app/branding", "readOnly": True})
        self.expect(m, "B", "mounts the branding")

    def test_unknown_secret_key(self):
        self.expect(lambda o: container(find(o, "Deployment", "hermit"), "hermit")["env"].append(
            {"name": "HERMIT_X", "valueFrom": {"secretKeyRef": {"name": "hermit", "key": "HERMIT_X"}}}), 2, "not in infra/secrets.txt")

    def test_max_unavailable(self):
        self.expect(lambda o: find(o, "Deployment", "tools")["spec"]["strategy"]["rollingUpdate"].update(maxUnavailable=1), 3, "maxUnavailable 0")

    def test_grace(self):
        self.expect(lambda o: find(o, "Deployment", "egress")["spec"]["template"]["spec"].update(terminationGracePeriodSeconds=150), 3, "< 625")

    def test_spread(self):
        self.expect(lambda o: find(o, "Deployment", "hermit")["spec"]["template"]["spec"]["topologySpreadConstraints"][0].update(whenUnsatisfiable="ScheduleAnyway"), 3, "DoNotSchedule")

    def test_egress_no_region(self):
        # egress's exit is yyc-nat1 wherever it runs (DESIGN D36): a planted fsn1 requirement is refused
        def m(o):
            t = find(o, "Deployment", "egress")["spec"]["template"]["spec"]["affinity"]["nodeAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"]["nodeSelectorTerms"][0]
            t["matchExpressions"].append({"key": "topology.kubernetes.io/region", "operator": "In", "values": ["fsn1"]})
        self.expect(m, "P", "must not require fsn1")

    def test_network(self):
        self.expect(lambda o: find(o, "Deployment", "magpie")["spec"]["template"]["spec"].pop("affinity"), "P", "network yyc")

    def test_bridge_prestop(self):
        self.expect(lambda o: container(find(o, "Deployment", "pigeon"), "pigeon")["lifecycle"]["preStop"]["sleep"].update(seconds=5), "P", "bridge callee")

    def test_pigeon_ephemeral(self):
        self.expect(lambda o: container(find(o, "Deployment", "pigeon"), "pigeon")["resources"]["limits"].update({"ephemeral-storage": "128Mi"}), "S", "evicts")

    def test_pigeon_init_memory(self):
        self.expect(lambda o: find(o, "Deployment", "pigeon")["spec"]["template"]["spec"]["initContainers"][0]["resources"]["limits"].update(memory="32Mi"), "S", "page cache")

    def test_gateway_release_is_an_aviary_release(self):
        # the gateway server is set up from this pin by hand (infra/gateway.sh): a branch name or a short sha would
        # only fail there, at the moment the gateway is being changed
        import shutil
        with tempfile.TemporaryDirectory() as d:
            folder = os.path.join(d, "deploy")
            shutil.copytree(FOLDER, folder)
            open(os.path.join(folder, "hermit", "gateway.release"), "w").write("main\n")
            with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
                json.dump(RENDER, f)
            r = subprocess.run([sys.executable, "-I", os.path.join(HERE, "check_deploy.py"), "--folder", folder,
                                "--render", f.name], capture_output=True, text=True)
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("::error::G gateway.release", r.stdout)

    def test_balancer_port_is_a_name(self):
        # the balancer's ports are told apart by their target port's name: a number would reach every selected pod
        def m(objs):
            find(objs, "Service", "egress-lb")["spec"]["ports"][0]["targetPort"] = 3128
        self.expect(m, "L", "is not a port name")

    def test_balancer_port_reaches_one_workload(self):
        # a second workload that declares the proxy's port name behind the balancer would get the sandboxes' proxy
        # connections too
        def m(objs):
            f = find(objs, "Deployment", "pg-forward")
            f["spec"]["template"]["metadata"]["labels"].update(find(objs, "Service", "egress-lb")["spec"]["selector"])
            container(f, "haproxy")["ports"].append({"name": "proxy", "containerPort": 3128})
        self.expect(m, "L", "exactly one")

    def test_policy_selector(self):
        self.expect(lambda o: find(o, "NetworkPolicy", "tools-in")["spec"]["podSelector"]["matchLabels"].update(app="toolz"), 6, "matches no workload")

    def test_skills_items(self):
        def m(o):
            v = next(v for v in find(o, "Deployment", "hermit")["spec"]["template"]["spec"]["volumes"] if v["name"] == "skills")
            v["configMap"]["items"].pop()
        self.expect(m, "K", "items")

    def test_skills_data(self):
        def m(o):
            cm = next(x for x in o if x and x["kind"] == "ConfigMap" and x["metadata"]["name"].startswith("hermit-skills"))
            cm["data"]["web.SKILL.md"] += "drift"
        self.expect(m, "K", "differs from hermit/skills/")

    def test_managed_size(self):
        def m(o):
            cm = next(x for x in o if x and x["kind"] == "ConfigMap" and x["metadata"]["name"].startswith("hermit-skills"))
            cm["data"]["web.SKILL.md"] += "x" * 240000
        self.expect(m, "K", "hermit refuses them")

    def test_host_folder_needs_a_reason(self):
        def m(o):
            find(o, "Deployment", "tools")["spec"]["template"]["spec"]["volumes"].append(
                {"name": "x", "hostPath": {"path": "/var/lib/anything"}})
        self.expect(m, "D", "/var/lib/anything")

    def test_old_namespace(self):
        def m(o):
            x = copy.deepcopy(find(o, "Service", "pigeon"))
            x["metadata"]["namespace"] = "jarvis"
            o.append(x)
        self.expect(m, 13, "namespace jarvis")

    def test_catalogue_from_a_configmap(self):
        # the access catalogue is people's addresses: a ConfigMap would have to be a file in git
        def m(o):
            v = next(v for v in find(o, "Deployment", "hermit")["spec"]["template"]["spec"]["volumes"] if v["name"] == "catalogue")
            v.pop("secret")
            v["configMap"] = {"name": "catalogue"}
        self.expect(m, "A", "volume catalogue is not a Secret")

    def test_catalogue_in_a_configmap(self):
        def m(o):
            o.append({"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": "catalogue", "namespace": "yyc-ai"},
                      "data": {"catalogue.json": json.dumps({"users": ["@example.com"], "admins": []})}})
        self.expect(m, "A", "an access catalogue in a ConfigMap")

    def test_catalogue_as_a_file(self):
        import shutil
        with tempfile.TemporaryDirectory() as d:
            folder = os.path.join(d, "deploy")
            shutil.copytree(FOLDER, folder)
            json.dump({"users": ["@example.com", "tony.leung@example.org"], "admins": []}, open(os.path.join(folder, "catalogue.json"), "w"))
            with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
                json.dump(RENDER, f)
            r = subprocess.run([sys.executable, "-I", os.path.join(HERE, "check_deploy.py"), "--folder", folder,
                                "--render", f.name], capture_output=True, text=True)
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("::error::A catalogue.json: an access catalogue is a file of the folder", r.stdout)


if __name__ == "__main__":
    unittest.main()
