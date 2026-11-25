//! One run's throwaway secrets and the files that name run-specific values (platform 7.1): pair tokens, hermit's
//! token keys, the fabric PKI (fabric/host/make-certs.sh), the harness CA with fake-access's and the stand-in EC2's
//! certificates, the Access key pair, hermit.json and its files, and the Kubernetes objects that carry them.
//!
//! Writes `<work>/k8s/*.yaml` (applied after testing/e2e/k8s) and `<work>/driver.env` (the driver's tokens). Nothing
//! here is a real credential: every value is new per run and dies with the cluster.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use base64::Engine as _;
use serde_json::{Value, json};

use crate::sh::{cmd, hex, random, write_private};
use crate::{Infra, NS};

const REGION: &str = "ap-east-1";

fn tok() -> Result<String> {
    hex(24)
}

fn b64url32() -> Result<String> {
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random(32)?))
}

fn b64_file(p: &Path) -> Result<String> {
    Ok(base64::engine::general_purpose::STANDARD.encode(std::fs::read(p).with_context(|| p.display().to_string())?))
}

fn read(p: &Path) -> Result<String> {
    std::fs::read_to_string(p).with_context(|| p.display().to_string())
}

/// A JSON string: YAML reads it as the same string.
fn quoted(s: &str) -> String {
    Value::String(s.to_owned()).to_string()
}

fn secret_env(out: &mut String, name: &str, kv: &[(&str, String)]) {
    let _ = write!(
        out,
        "apiVersion: v1\nkind: Secret\nmetadata: {{name: {name}, namespace: {NS}}}\ntype: Opaque\nstringData:\n"
    );
    for (k, v) in kv {
        let _ = writeln!(out, "  {k}: {}", quoted(v));
    }
    out.push_str("---\n");
}

fn secret_files(out: &mut String, name: &str, kv: &[(&str, &Path)]) -> Result<()> {
    let _ = write!(
        out,
        "apiVersion: v1\nkind: Secret\nmetadata: {{name: {name}, namespace: {NS}}}\ntype: Opaque\ndata:\n"
    );
    for (k, p) in kv {
        let _ = writeln!(out, "  {k}: {}", b64_file(p)?);
    }
    out.push_str("---\n");
    Ok(())
}

/// A leaf certificate of the harness CA in `ca` for `dns`: `<name>.crt` and its PKCS#8 key `<name>.key`.
fn leaf(ca: &Path, name: &str, dns: &str) -> Result<()> {
    let p = |s: &str| ca.join(s).to_string_lossy().into_owned();
    cmd(
        "openssl",
        &[
            "req",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            &p(&format!("{name}.key.ec")),
            "-out",
            &p(&format!("{name}.csr")),
            "-subj",
            &format!("/CN={dns}"),
        ],
    )
    .run(60)?;
    cmd(
        "openssl",
        &[
            "pkcs8",
            "-topk8",
            "-nocrypt",
            "-in",
            &p(&format!("{name}.key.ec")),
            "-out",
            &p(&format!("{name}.key")),
        ],
    )
    .run(60)?;
    std::fs::write(
        ca.join(format!("{name}.ext")),
        format!("subjectAltName=DNS:{dns}\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:FALSE\n"),
    )?;
    cmd(
        "openssl",
        &[
            "x509",
            "-req",
            "-in",
            &p(&format!("{name}.csr")),
            "-CA",
            &p("ca.crt"),
            "-CAkey",
            &p("ca.key"),
            "-CAcreateserial",
            "-days",
            "2",
            "-extfile",
            &p(&format!("{name}.ext")),
            "-out",
            &p(&format!("{name}.crt")),
        ],
    )
    .run(60)?;
    Ok(())
}

/// Every generated file under `work`, for the sandbox image `digest`, Postgres at `pg` and S3 at `s3` on the
/// Docker network `subnet`.
#[allow(clippy::too_many_lines)] // one run's every secret, as one list
pub fn generate(here: &Path, work: &Path, infra: &Infra, digest: &str, subnet: &str, pg: &str, s3: &str) -> Result<()> {
    let out = work.join("k8s");
    std::fs::create_dir_all(&out)?;
    // --- pair tokens (platform 3.7: <CALLER>_TOKEN_<CALLEE> = <CALLEE>_ACCEPT_<CALLER>)
    let (pigeon_magpie, pigeon_hermit, pigeon_model) = (tok()?, tok()?, tok()?);
    let magpie_hermit = tok()?;
    let (hermit_magpie, hermit_egress) = (tok()?, tok()?);
    let (gateway_magpie, gateway_hermit, gateway_tools, model_key) = (tok()?, tok()?, tok()?, tok()?);
    let (admin_magpie, admin_hermit, monitor_hermit, tools_hermit) = (tok()?, tok()?, tok()?, tok()?);

    // --- PKI: the fabric's (hermit <-> sandboxd) and the harness CA (fake-access, EC2's name)
    let fabric = work.join("fabric");
    cmd(
        &here.join("../../fabric/host/make-certs.sh").to_string_lossy(),
        &[&fabric.to_string_lossy()],
    )
    .run(120)?;
    let ca = work.join("ca");
    std::fs::create_dir_all(&ca)?;
    let c = |s: &str| ca.join(s).to_string_lossy().into_owned();
    cmd(
        "openssl",
        &[
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            &c("ca.key"),
            "-out",
            &c("ca.crt"),
            "-days",
            "2",
            "-subj",
            "/CN=aviary-e2e CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
        ],
    )
    .run(60)?;
    leaf(&ca, "access", &format!("access.{NS}.svc.cluster.local"))?;
    leaf(&ca, "ec2", &format!("ec2.{REGION}.amazonaws.com"))?;
    for k in ["k1", "k2"] {
        cmd(
            "openssl",
            &[
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
                "-out",
                &c(&format!("{k}.pem")),
            ],
        )
        .run(60)?;
    }
    // the fabric's server keys in PKCS#8 (rustls reads either; one form throughout)
    for l in ["gateway", "host", "hermit"] {
        let f = |s: &str| fabric.join(s).to_string_lossy().into_owned();
        cmd(
            "openssl",
            &[
                "pkcs8",
                "-topk8",
                "-nocrypt",
                "-in",
                &f(&format!("{l}.key")),
                "-out",
                &f(&format!("{l}.p8")),
            ],
        )
        .run(60)?;
    }

    let pg_url = |db: &str| format!("postgres://{db}:{}@{pg}:5432/{db}?sslmode=require", infra.pg_password);
    let jfs_meta = format!("postgres://jfs@{pg}:5432/jfs?sslmode=require");

    // --- hermit.json (testing/e2e/hermit.json with this run's image)
    let mut hermit: Value = serde_json::from_str(&read(&here.join("hermit.json"))?)?;
    hermit["sandbox"]["image"] = json!(format!("docker.io/aviary-e2e/hermit-sandbox:e2e@{digest}"));
    write_private(&work.join("hermit.json"), serde_json::to_string_pretty(&hermit)?)?;

    // --- the gateway host's config (hermit's user-data shape, sandboxd role hub)
    let gw = json!({
        "host": {"name": "hermit-gw", "role": "hub", "gateway": true, "wgAddress": "10.52.0.1", "reserveMemory": 0},
        "wgPrivateKey": base64::engine::general_purpose::STANDARD.encode(random(32)?),
        "tls": {"ca": read(&fabric.join("ca.crt"))?, "cert": read(&fabric.join("gateway.crt"))?,
                "key": read(&fabric.join("gateway.p8"))?},
    });
    write_private(&work.join("gateway-host.json"), serde_json::to_string_pretty(&gw)?)?;

    let mut s = String::new();
    secret_env(
        &mut s,
        "pigeon-env",
        &[
            ("PIGEON_TOKEN_MAGPIE", pigeon_magpie.clone()),
            ("PIGEON_TOKEN_HERMIT", pigeon_hermit.clone()),
            ("PIGEON_TOKEN_MODEL", pigeon_model.clone()),
            ("PIGEON_VIEW_AS_KEYS", b64url32()?),
            ("PIGEON_BLOB_KEYS", b64url32()?),
        ],
    );
    secret_env(
        &mut s,
        "magpie-env",
        &[
            ("MAGPIE_DATABASE_URL", pg_url("magpie")),
            ("MAGPIE_S3_ENDPOINT", format!("http://{s3}:9000")),
            ("MAGPIE_S3_REGION", "us-east-1".into()),
            ("MAGPIE_S3_BUCKET", "magpie".into()),
            ("MAGPIE_S3_ACCESS_KEY_ID", infra.s3_access.clone()),
            ("MAGPIE_S3_SECRET_ACCESS_KEY", infra.s3_secret.clone()),
            ("MAGPIE_ACCEPT_PIGEON", pigeon_magpie.clone()),
            ("MAGPIE_ACCEPT_GATEWAY", gateway_magpie.clone()),
            ("MAGPIE_ACCEPT_HERMIT", hermit_magpie.clone()),
            ("MAGPIE_ACCEPT_ADMIN", admin_magpie.clone()),
            ("MAGPIE_TOKEN_HERMIT", magpie_hermit.clone()),
        ],
    );
    secret_env(
        &mut s,
        "hermit-env",
        &[
            ("HERMIT_DATABASE_URL", pg_url("hermit")),
            ("HERMIT_TOKEN_KEYS", format!("1:{}", b64url32()?)),
            ("HERMIT_ACCEPT_PIGEON", pigeon_hermit),
            ("HERMIT_ACCEPT_MAGPIE", magpie_hermit.clone()),
            ("HERMIT_ACCEPT_GATEWAY", gateway_hermit.clone()),
            ("HERMIT_ACCEPT_TOOLS", tools_hermit),
            ("HERMIT_ACCEPT_MONITOR", monitor_hermit.clone()),
            ("HERMIT_ACCEPT_ADMIN", admin_hermit.clone()),
            ("HERMIT_TOKEN_MAGPIE", hermit_magpie),
            ("HERMIT_TOKEN_EGRESS", hermit_egress.clone()),
            ("HERMIT_FABRIC_CA", read(&fabric.join("ca.crt"))?.trim_end().to_owned()),
            (
                "HERMIT_FABRIC_CLIENT_CERT",
                read(&fabric.join("hermit.crt"))?.trim_end().to_owned(),
            ),
            (
                "HERMIT_FABRIC_CLIENT_KEY",
                read(&fabric.join("hermit.p8"))?.trim_end().to_owned(),
            ),
            (
                "HERMIT_FABRIC_HOST_CERT",
                read(&fabric.join("host.crt"))?.trim_end().to_owned(),
            ),
            (
                "HERMIT_FABRIC_HOST_KEY",
                read(&fabric.join("host.p8"))?.trim_end().to_owned(),
            ),
            ("HERMIT_AWS_ACCESS_KEY_ID", "AKIAE2E0000000000000".into()),
            ("HERMIT_AWS_SECRET_ACCESS_KEY", tok()?),
            ("HERMIT_JUICEFS_META_URL", jfs_meta),
            ("HERMIT_JUICEFS_META_PASSWORD", infra.jfs_password.clone()),
        ],
    );
    secret_env(
        &mut s,
        "gateway-env",
        &[
            ("GATEWAY_TOKEN_MAGPIE", gateway_magpie),
            ("GATEWAY_TOKEN_HERMIT", gateway_hermit),
            ("GATEWAY_TOKEN_TOOLS", gateway_tools.clone()),
            ("GATEWAY_MODEL_KEY", model_key),
            ("GATEWAY_ACCEPT_PIGEON", pigeon_model),
            ("GATEWAY_ACCEPT_HERMIT", hermit_egress),
        ],
    );
    secret_env(&mut s, "tools-token", &[("token", gateway_tools)]);
    secret_files(&mut s, "harness-ca", &[("ca.crt", &ca.join("ca.crt"))])?;
    secret_files(
        &mut s,
        "access-keys",
        &[
            ("k1.pem", &ca.join("k1.pem")),
            ("k2.pem", &ca.join("k2.pem")),
            ("tls.crt", &ca.join("access.crt")),
            ("tls.key", &ca.join("access.key")),
        ],
    )?;
    secret_files(
        &mut s,
        "fake-ec2-tls",
        &[("tls.crt", &ca.join("ec2.crt")), ("tls.key", &ca.join("ec2.key"))],
    )?;
    secret_files(
        &mut s,
        "fabric-gateway",
        &[("host.json", &work.join("gateway-host.json"))],
    )?;
    let driver = [
        ("E2E_MAGPIE_ADMIN", admin_magpie),
        ("E2E_HERMIT_ADMIN", admin_hermit),
        ("E2E_HERMIT_MONITOR", monitor_hermit),
        ("E2E_MAGPIE_PIGEON", pigeon_magpie),
        ("E2E_HERMIT_MAGPIE", magpie_hermit),
    ];
    secret_env(&mut s, "driver-env", &driver);
    secret_files(&mut s, "driver-ca", &[("ca.crt", &ca.join("ca.crt"))])?;
    write_private(&out.join("secrets.yaml"), s)?;

    let mut cfg =
        format!("apiVersion: v1\nkind: ConfigMap\nmetadata: {{name: hermit-config, namespace: {NS}}}\ndata:\n");
    for f in [
        work.join("hermit.json"),
        here.join("hermit/system-prompt.md"),
        here.join("hermit/sandbox-rules.md"),
        here.join("hermit/title-system.md"),
    ] {
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        let _ = writeln!(cfg, "  {name}: {}", quoted(&read(&f)?));
    }
    let _ = write!(
        cfg,
        "---\napiVersion: v1\nkind: ConfigMap\nmetadata: {{name: hermit-catalogue, namespace: {NS}}}\ndata:\n  catalogue.json: {}\n",
        quoted(&read(&here.join("hermit/catalogue.json"))?)
    );
    write_private(&out.join("config.yaml"), cfg)?;

    // Postgres and S3 run beside the cluster on its Docker network; the API server is the node's :6443
    write_private(
        &out.join("external.yaml"),
        format!(
            "apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {{name: external, namespace: {NS}}}
spec:
  podSelector: {{matchExpressions: [{{key: app, operator: In, values: [magpie, hermit]}}]}}
  policyTypes: [Egress]
  egress:
    - to: [{{ipBlock: {{cidr: {pg}/32}}}}]
      ports: [{{port: 5432}}]
    - to: [{{ipBlock: {{cidr: {s3}/32}}}}]
      ports: [{{port: 9000}}]
---
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {{name: api-server, namespace: {NS}}}
spec:
  podSelector: {{matchExpressions: [{{key: app, operator: In, values: [hermit, fake-ec2, fabric-gateway, fake-host, driver]}}]}}
  policyTypes: [Egress]
  egress:
    - to: [{{ipBlock: {{cidr: {subnet}}}}}]
      ports: [{{port: 6443}}]
---
apiVersion: v1
kind: ConfigMap
metadata: {{name: coredns-custom, namespace: kube-system}}
data:
  # hermit's EC2 client calls EC2's regional name; in the harness that name is the stand-in
  ec2.override: |
    rewrite name exact ec2.{REGION}.amazonaws.com fake-ec2.{NS}.svc.cluster.local
"
        ),
    )?;

    let mut env = String::new();
    for (k, v) in &driver {
        let _ = writeln!(env, "{k}={v}");
    }
    write_private(&work.join("driver.env"), env)?;
    Ok(())
}
