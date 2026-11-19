#!/usr/bin/env python3
"""infra/seal.py's checks on seeded trees (stdlib only; run by yyc-ai.yml check): the cert by folder, a wrong-cert
file refused, A|B? values, --allow-unsealed. Builds a throwaway repo root with copies of seal.py and both certs."""
import os, shutil, subprocess, sys, tempfile, unittest

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(REPO, "infra"))
import seal as SEAL  # noqa: E402
sys.path.pop(0)
NEW = SEAL.CERTS[0][0].rstrip("/")   # the new stack's sealed folder (yyc-ai/next/sealed, after promotion yyc-ai/deploy/sealed)


def tree(lines, files):
    root = tempfile.mkdtemp()
    os.makedirs(os.path.join(root, "infra"))
    for f in ("seal.py", "sealed-secrets.pem", "sealed-secrets-talos.pem"):
        shutil.copy(os.path.join(REPO, "infra", f), os.path.join(root, "infra", f))
    open(os.path.join(root, "infra", "secrets.txt"), "w").write("\n".join(lines) + "\n")
    for d in ("news-stream/deploy/apps/sealed", NEW, "news-stream/deploy/clusters/talos/db/sealed"):
        os.makedirs(os.path.join(root, d))
        listed = [os.path.basename(p) for p in files if os.path.dirname(p) == d]
        body = "resources:\n" + "".join(f"  - {b}\n" for b in listed) if listed else "resources: []\n"
        open(os.path.join(root, d, "kustomization.yaml"), "w").write(body)
    for p, header in files.items():
        open(os.path.join(root, p), "w").write(header + "apiVersion: bitnami.com/v1alpha1\nkind: SealedSecret\n")
    return root


def check(root, *flags):
    return subprocess.run([sys.executable, "-I", os.path.join(root, "infra", "seal.py"), "--check", "--names-only", *flags],
                          capture_output=True, text=True)


def cert_header(root, cert):
    sys.path.insert(0, os.path.join(root, "infra"))
    import importlib
    seal = importlib.import_module("seal")
    importlib.reload(seal)
    sys.path.pop(0)
    return seal.HEADER + seal.cert_line(cert)


class SealCheck(unittest.TestCase):
    def test_folder_cert(self):
        new, old = f"{NEW}/a.yaml", "news-stream/deploy/apps/sealed/b.yaml"
        lines = [f"yyc-ai/a Opaque {new} K='x'", f"yyc/b Opaque {old} K='y'"]
        root = tree(lines, {new: "", old: ""})
        for p, c in ((new, "infra/sealed-secrets-talos.pem"), (old, None)):
            if c:
                open(os.path.join(root, p), "w").write(cert_header(root, c) + "kind: SealedSecret\n")
        r = check(root)
        self.assertEqual(r.returncode, 0, r.stdout)

    def test_wrong_cert_refused(self):
        new = f"{NEW}/a.yaml"
        root = tree([f"yyc-ai/a Opaque {new} K='x'"], {new: ""})  # no cert line = the old cert
        r = check(root)
        self.assertEqual(r.returncode, 1)
        self.assertIn("wrong cert", r.stdout)
        talos_db = "news-stream/deploy/clusters/talos/db/sealed/c.yaml"
        old = "news-stream/deploy/apps/sealed/b.yaml"
        root = tree([f"db/c Opaque {talos_db} K='x'", f"yyc/b Opaque {old} K='y'"], {talos_db: "", old: ""})
        open(os.path.join(root, old), "w").write(cert_header(root, "infra/sealed-secrets-talos.pem") + "kind: SealedSecret\n")
        open(os.path.join(root, talos_db), "w").write(cert_header(root, "infra/sealed-secrets-talos.pem") + "kind: SealedSecret\n")
        r = check(root)
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"wrong cert: {old}", r.stdout)
        self.assertNotIn(f"wrong cert: {talos_db}", r.stdout)

    def test_unsealed(self):
        new = f"{NEW}/a.yaml"
        root = tree([f"yyc-ai/a Opaque {new} K='x'"], {})
        self.assertEqual(check(root).returncode, 1)
        r = check(root, "--allow-unsealed")
        self.assertEqual(r.returncode, 0, r.stdout)
        self.assertIn("not sealed yet", r.stdout)

    def test_join(self):
        seal = SEAL
        self.assertEqual(seal.resolve("A|B?", {"A": "1", "B": "2"}), "1,2")
        self.assertEqual(seal.resolve("A|B?", {"A": "1"}), "1")
        self.assertIsNone(seal.resolve("A|B?", {"B": "2"}))
        self.assertEqual(seal.resolve("'lit'", {}), "lit")
        self.assertEqual(seal.required_keys("A|B?|C"), ["A", "C"])
        self.assertEqual(seal.cert_for(f"{NEW}/x.yaml"), seal.TALOS_CERT)
        self.assertEqual(seal.cert_for("news-stream/deploy/clusters/talos/db/sealed/x.yaml"), seal.TALOS_CERT)
        self.assertEqual(seal.cert_for("news-stream/deploy/apps/sealed/x.yaml"), seal.OLD_CERT)


class AnotherTree(unittest.TestCase):
    def test_a_tree_and_map_elsewhere_checked_with_this_repositorys_certs(self):
        """DE0CH/aviary's yyc/deploy/sealed with its yyc/secrets.txt (--root, --map): the Talos cert, from this repository"""
        code = tree([], {})
        other = tempfile.mkdtemp()
        d = os.path.join(other, "yyc", "deploy", "sealed")
        os.makedirs(d)
        open(os.path.join(d, "kustomization.yaml"), "w").write("resources:\n  - a.yaml\n")
        open(os.path.join(other, "yyc", "secrets.txt"), "w").write("yyc-ai/a Opaque yyc/deploy/sealed/a.yaml K='x'\n")
        run = lambda: subprocess.run([sys.executable, "-I", os.path.join(code, "infra", "seal.py"), "--check", "--names-only",
                                      "--root", other, "--map", os.path.join(other, "yyc", "secrets.txt")],
                                     capture_output=True, text=True)
        open(os.path.join(d, "a.yaml"), "w").write(cert_header(code, "infra/sealed-secrets-talos.pem") + "kind: SealedSecret\n")
        r = run()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        open(os.path.join(d, "a.yaml"), "w").write(cert_header(code, "infra/sealed-secrets.pem") + "kind: SealedSecret\n")
        r = run()
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("wrong cert", r.stdout)


if __name__ == "__main__":
    unittest.main()
