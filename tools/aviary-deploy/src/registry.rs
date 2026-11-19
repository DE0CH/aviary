//! Whether an image exists at a tag on GHCR, and its digest: the public packages answer an anonymous pull token.

use anyhow::{Context, Result, bail};

const ACCEPT: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";

/// `ghcr.io/de0ch/<image>:<tag>` → its digest, or None when the tag is absent
pub fn digest(image: &str, tag: &str) -> Result<Option<String>> {
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .new_agent();
    let tok: serde_json::Value = agent
        .get(&format!("https://ghcr.io/token?scope=repository:de0ch/{image}:pull"))
        .call()
        .context("ghcr token")?
        .body_mut()
        .read_json()
        .context("ghcr token body")?;
    let Some(tok) = tok["token"].as_str() else {
        bail!("ghcr gave no token for {image}")
    };
    let r = agent
        .head(&format!("https://ghcr.io/v2/de0ch/{image}/manifests/{tag}"))
        .header("Authorization", &format!("Bearer {tok}"))
        .header("Accept", ACCEPT)
        .call()
        .with_context(|| format!("ghcr manifest {image}:{tag}"))?;
    match r.status().as_u16() {
        200 => Ok(r
            .headers()
            .get("docker-content-digest")
            .and_then(|v| v.to_str().ok())
            .map(String::from)),
        404 => Ok(None),
        s => bail!("ghcr manifest {image}:{tag}: HTTP {s}"),
    }
}
