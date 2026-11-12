//! `hermit.json` (hermit 9): every YYC value besides the catalogue, loaded once at start and validated strictly
//! (an unknown key is an error, so a typo fails the rollout's first pod and the old pods keep serving), plus the
//! three text files it names (the managed CLAUDE.md's two parts and the title instructions).
//!
//! The `fabric` section is parsed here so the whole file is checked at start; the fabric (WP-19) is its reader.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// Where the `ConfigMap` `hermit-config` is mounted (hermit 9).
pub const CONFIG_PATH: &str = "/etc/hermit/config/hermit.json";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{path}: {source}")]
    Read { path: String, source: std::io::Error },
    #[error("hermit.json: {0}")]
    Parse(String),
    #[error("hermit.json: {0}")]
    Invalid(String),
}

fn invalid(m: impl Into<String>) -> ConfigError {
    ConfigError::Invalid(m.into())
}

/// The file as written.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HermitJson {
    pub product: Product,
    pub sign_in: SignIn,
    pub catalogue: PathBuf,
    pub prompt: Prompt,
    pub sandbox: SandboxJson,
    pub fabric: FabricJson,
    pub ops: OpsJson,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Product {
    pub name: String,
    pub locale: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignIn {
    pub access: Vec<AccessApp>,
    pub app_scheme: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessApp {
    pub team_domain: String,
    pub audience: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Prompt {
    pub instructions: PathBuf,
    pub rules: PathBuf,
    pub title: PathBuf,
    /// The managed skills' folder (decision 15): `<skill>/<file>` two levels down, each file the text of the
    /// descriptor's `claude.managedSkills["<skill>/<file>"]`. Names starting with `.` are skipped (a mounted
    /// `ConfigMap`'s `..data`); a deeper folder is refused. Absent = no managed skills.
    #[serde(default)]
    pub skills: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxJson {
    /// `<repository>@sha256:<64 hex>`.
    pub image: String,
    pub cpu: f32,
    pub memory: String,
    pub pids: u32,
    pub open_files: u32,
    pub max_active: u32,
    pub idle_minutes: u32,
    pub max_lifetime_hours: u32,
    pub lifetime_grace_minutes: u32,
    pub capacity_wait_seconds: u32,
    pub host_wait_seconds: u32,
    pub supervisor_wait_seconds: u32,
    pub start_deadline_seconds: u32,
    pub pool_victim_idle_seconds: u32,
    pub reserve_seconds: u32,
    pub activity_every_seconds: u32,
    pub workspace: WorkspaceJson,
    #[serde(rename = "tmpGiB")]
    pub tmp_gib: u32,
    /// The boot environment's proxy URL, with `{token}` and `{vip}`.
    pub proxy: String,
    /// The descriptor's `env`, with `{token}` and `{vip}` filled in.
    pub env: BTreeMap<String, String>,
    /// The descriptor's `claude.compactAtImages`: the conversation is compacted at a turn's end once it holds this
    /// many images (at least 1). Absent = never.
    #[serde(default)]
    pub compact_at_images: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceJson {
    #[serde(rename = "quotaGiB")]
    pub quota_gib: u32,
    pub quota_inodes: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FabricJson {
    pub sandbox_cidr: String,
    pub host_cidr: String,
    pub egress_vip: std::net::Ipv4Addr,
    pub control_cidr: String,
    pub pod_cidr: String,
    pub egress_ports: Vec<EgressPort>,
    pub egress_service: String,
    pub gateway: GatewayJson,
    pub sandboxd: String,
    pub runsc: String,
    pub cpu_features: String,
    pub cpu_overcommit: u32,
    pub spare_slots: u32,
    pub min_available_memory: String,
    pub min_host_lifetime_seconds: u32,
    pub host_dead_seconds: u32,
    pub gateway_dead_seconds: u32,
    pub gateway_settle_seconds: u32,
    pub boot_timeout_seconds: u32,
    pub orphan_instance_seconds: u32,
    pub jfs_retire_seconds: u32,
    pub migration: MigrationJson,
    pub aws: AwsJson,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EgressPort {
    pub vip: u16,
    pub target: u16,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayJson {
    pub name: String,
    pub url: String,
    pub endpoint: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationJson {
    pub downtime_ms: u32,
    pub max_rounds: u32,
    pub max_downtime_ms: u32,
    pub idle_cpu: f64,
    pub min_age_seconds: u32,
    /// A sandbox with more than this in use on its scratch filesystem is not moved (13.10 `movable`): the final
    /// scratch delta compares contents (rsync `--checksum`) while the sandbox is stopped, about 3.2 ms per MiB in use
    /// (sandboxd tests/real-host), so 512 MiB adds about 1.7 s to the pause. An interim bound until the owner of 13
    /// decides between it and a narrower checksum (WORK-PACKAGES WP-19 note); 0 = no bound.
    #[serde(default = "max_scratch_mib")]
    pub max_scratch_mib: u32,
}

fn max_scratch_mib() -> u32 {
    512
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AwsJson {
    pub region: String,
    pub launch_template: String,
    pub max_hosts: u32,
    pub subnet: String,
    pub tags: BTreeMap<String, String>,
    pub fallback_host: FallbackHost,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FallbackHost {
    pub memory: String,
    pub reserve: String,
    pub cpus: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpsJson {
    pub time_zone: String,
    pub deployments: Vec<String>,
    pub pod_node_prefix: String,
    pub budget_watch: f64,
}

/// hermit's configuration as the core uses it: the file, checked, with its derived values and the text files read.
#[derive(Clone, Debug)]
pub struct Config {
    pub file: HermitJson,
    /// `sha256:<64 hex>`, the pinned digest of `sandbox.image`.
    pub image_digest: String,
    pub memory_bytes: u64,
    pub workspace_quota_bytes: u64,
    pub tmp_bytes: u64,
    /// The instance prompt + a blank line + the sandbox rules with their placeholders filled (3.7.1).
    pub managed_claude_md: String,
    /// The managed skills, `<skill>/<file>` → text (decision 15; the descriptor's `claude.managedSkills`).
    pub managed_skills: BTreeMap<String, String>,
    pub title_system: String,
}

impl Config {
    /// Reads and checks `hermit.json` and the files it names.
    ///
    /// # Errors
    /// An unreadable file, a parse error (an unknown key included) or a value that fails a rule.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = read(path)?;
        let file: HermitJson = serde_json::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let instructions = read(&file.prompt.instructions)?;
        let rules = read(&file.prompt.rules)?;
        let title = read(&file.prompt.title)?;
        let skills = match &file.prompt.skills {
            Some(dir) => read_skills(dir)?,
            None => BTreeMap::new(),
        };
        Self::from_parts(file, &instructions, &rules, &title)?.with_skills(skills)
    }

    /// The configuration with these managed skills, checked: every key `<skill>/<file>`, and the managed content
    /// within [`hermit_api::sandbox_surface::MANAGED_MAX_BYTES`].
    ///
    /// # Errors
    /// A bad key, or the content over the bound.
    pub fn with_skills(mut self, skills: BTreeMap<String, String>) -> Result<Self, ConfigError> {
        if let Some(bad) = skills
            .keys()
            .find(|k| !hermit_api::sandbox_surface::valid_skill_path(k))
        {
            return Err(invalid(format!(
                "prompt.skills: {bad:?} is not <skill>/<file> ([A-Za-z0-9][A-Za-z0-9._-]* each)"
            )));
        }
        self.managed_skills = skills;
        let size = serde_json::to_string(&(&self.managed_claude_md, &self.managed_skills, &self.title_system))
            .map_or(usize::MAX, |s| s.len());
        let max = hermit_api::sandbox_surface::MANAGED_MAX_BYTES;
        if size > max {
            return Err(invalid(format!(
                "the managed CLAUDE.md, skills and title instructions are {size} bytes as JSON, over {max}"
            )));
        }
        Ok(self)
    }

    /// The configuration from an already parsed file and the three texts (tests build it this way).
    ///
    /// # Errors
    /// A value that fails a rule.
    pub fn from_parts(file: HermitJson, instructions: &str, rules: &str, title: &str) -> Result<Self, ConfigError> {
        let s = &file.sandbox;
        let image_digest =
            image_digest(&s.image).ok_or_else(|| invalid("sandbox.image must be pinned as <repo>@sha256:<64 hex>"))?;
        let memory_bytes = parse_quantity(&s.memory).ok_or_else(|| invalid("sandbox.memory: a quantity like 1Gi"))?;
        if memory_bytes == 0 {
            return Err(invalid("sandbox.memory must be positive"));
        }
        if !(s.cpu > 0.0 && s.cpu.is_finite()) {
            return Err(invalid("sandbox.cpu must be positive"));
        }
        for (name, v) in [
            ("maxActive", s.max_active),
            ("idleMinutes", s.idle_minutes),
            ("maxLifetimeHours", s.max_lifetime_hours),
            ("activityEverySeconds", s.activity_every_seconds),
            ("startDeadlineSeconds", s.start_deadline_seconds),
            ("supervisorWaitSeconds", s.supervisor_wait_seconds),
            ("pids", s.pids),
            ("openFiles", s.open_files),
        ] {
            if v == 0 {
                return Err(invalid(format!("sandbox.{name} must be positive")));
            }
        }
        if !s.proxy.contains("{token}") || !s.proxy.contains("{vip}") {
            return Err(invalid("sandbox.proxy must contain {token} and {vip}"));
        }
        if s.env.len() + 6 > hermit_api::sandbox_surface::ENV_MAX_ENTRIES {
            return Err(invalid("sandbox.env has too many entries"));
        }
        for k in s.env.keys() {
            if k.is_empty()
                || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || k.as_bytes()[0].is_ascii_digit()
            {
                return Err(invalid(format!("sandbox.env: {k:?} is not a variable name")));
            }
        }
        if s.compact_at_images == Some(0) {
            return Err(invalid(
                "sandbox.compactAtImages must be at least 1 (leave it out for never)",
            ));
        }
        if file.product.name.trim().is_empty() || file.product.locale.trim().is_empty() {
            return Err(invalid("product.name and product.locale must be set"));
        }
        for m in [
            &file.fabric.min_available_memory,
            &file.fabric.aws.fallback_host.memory,
            &file.fabric.aws.fallback_host.reserve,
        ] {
            parse_quantity(m).ok_or_else(|| invalid(format!("fabric: {m:?} is not a quantity")))?;
        }
        let workspace_quota_bytes = u64::from(s.workspace.quota_gib) << 30;
        let tmp_bytes = u64::from(s.tmp_gib) << 30;
        let rules = rules
            .replace("{workspaceGiB}", &s.workspace.quota_gib.to_string())
            .replace("{workspaceFiles}", &thousands(s.workspace.quota_inodes))
            .replace("{tmpGiB}", &s.tmp_gib.to_string());
        let managed_claude_md = format!("{}\n\n{}", instructions.trim_end_matches('\n'), rules);
        Ok(Self {
            image_digest,
            memory_bytes,
            workspace_quota_bytes,
            tmp_bytes,
            managed_claude_md,
            managed_skills: BTreeMap::new(),
            title_system: title.to_owned(),
            file,
        })
    }

    #[must_use]
    pub fn sandbox(&self) -> &SandboxJson {
        &self.file.sandbox
    }

    #[must_use]
    pub fn secs(v: u32) -> Duration {
        Duration::from_secs(u64::from(v))
    }

    /// `ceil(cpu)`, the descriptor's `resources.cpus`.
    #[must_use]
    pub fn cpus(&self) -> u32 {
        // cpu is positive and small (a few cores)
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = self.file.sandbox.cpu.ceil() as u32;
        n.max(1)
    }

    /// `{token}` and `{vip}` filled into a template.
    #[must_use]
    pub fn fill(&self, template: &str, token: &str) -> String {
        template
            .replace("{token}", token)
            .replace("{vip}", &self.file.fabric.egress_vip.to_string())
    }
}

/// `dir/<skill>/<file>` → text; hidden names skipped, a deeper folder refused (symlinks followed, as a mounted
/// `ConfigMap`'s entries are links into `..data`).
fn read_skills(dir: &Path) -> Result<BTreeMap<String, String>, ConfigError> {
    let err = |path: &Path, source: std::io::Error| ConfigError::Read {
        path: path.display().to_string(),
        source,
    };
    let visible = |dir: &Path| -> Result<Vec<(String, PathBuf)>, ConfigError> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).map_err(|e| err(dir, e))? {
            let e = e.map_err(|e| err(dir, e))?;
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with('.') {
                out.push((name, e.path()));
            }
        }
        Ok(out)
    };
    let mut skills = BTreeMap::new();
    for (skill, sdir) in visible(dir)? {
        if !std::fs::metadata(&sdir).map_err(|e| err(&sdir, e))?.is_dir() {
            return Err(invalid(format!(
                "prompt.skills: {} is not a skill folder",
                sdir.display()
            )));
        }
        for (file, path) in visible(&sdir)? {
            if std::fs::metadata(&path).map_err(|e| err(&path, e))?.is_dir() {
                return Err(invalid(format!(
                    "prompt.skills: {} is a folder (a skill holds files only)",
                    path.display()
                )));
            }
            skills.insert(format!("{skill}/{file}"), read(&path)?);
        }
    }
    Ok(skills)
}

fn read(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })
}

/// `sha256:<64 hex>` from `<repo>@sha256:<64 hex>`.
#[must_use]
pub fn image_digest(image: &str) -> Option<String> {
    let (repo, digest) = image.rsplit_once('@')?;
    let hex = digest.strip_prefix("sha256:")?;
    (!repo.is_empty() && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
        .then(|| digest.to_owned())
}

/// A Kubernetes-style quantity: plain bytes or with `Ki`, `Mi`, `Gi`, `Ti` (decimals allowed: `15.5Gi`).
#[must_use]
pub fn parse_quantity(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = [("Ki", 1u64 << 10), ("Mi", 1 << 20), ("Gi", 1 << 30), ("Ti", 1 << 40)]
        .iter()
        .find_map(|(suffix, m)| s.strip_suffix(suffix).map(|n| (n, *m)))
        .unwrap_or((s, 1));
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit() || b == b'.') || num.matches('.').count() > 1 {
        return None;
    }
    let (int, frac) = num.split_once('.').unwrap_or((num, ""));
    let int: u64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let mut total = int.checked_mul(mult)?;
    if !frac.is_empty() {
        let digits = u32::try_from(frac.len()).ok()?;
        let f: u64 = frac.parse().ok()?;
        let scale = 10u64.checked_pow(digits)?;
        total = total.checked_add(f.checked_mul(mult)? / scale)?;
    }
    Some(total)
}

/// `1000` -> `1,000` (the rules text's own form).
#[must_use]
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    pub(crate) const EXAMPLE: &str = include_str!("../tests/fixtures/hermit.json");

    #[test]
    fn quantities() {
        assert_eq!(parse_quantity("1Gi"), Some(1 << 30));
        assert_eq!(parse_quantity("256Mi"), Some(256 << 20));
        assert_eq!(parse_quantity("15.5Gi"), Some((31u64 << 30) / 2));
        assert_eq!(parse_quantity("1024"), Some(1024));
        assert_eq!(parse_quantity("1.2.3Gi"), None);
        assert_eq!(parse_quantity("Gi"), None);
        assert_eq!(parse_quantity("-1Gi"), None);
    }

    #[test]
    fn digests_and_numbers() {
        let h = "a".repeat(64);
        assert_eq!(
            image_digest(&format!("r.example/x@sha256:{h}")),
            Some(format!("sha256:{h}"))
        );
        assert_eq!(image_digest("r.example/x:latest"), None);
        assert_eq!(image_digest(&format!("r.example/x@sha256:{}", "A".repeat(64))), None);
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn the_example_loads_and_unknown_keys_are_refused() {
        let file: HermitJson = serde_json::from_str(EXAMPLE).unwrap();
        let c = Config::from_parts(
            file,
            "Be helpful.\n",
            "Up to {workspaceGiB} GiB, {workspaceFiles} files, {tmpGiB} GiB.",
            "t",
        )
        .unwrap();
        assert_eq!(c.managed_claude_md, "Be helpful.\n\nUp to 1 GiB, 1,000 files, 5 GiB.");
        assert_eq!(c.memory_bytes, 1 << 30);
        assert!(c.image_digest.starts_with("sha256:"));
        let mut v: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
        v["sandbox"]["idleMinute"] = 3.into();
        let e = serde_json::from_value::<HermitJson>(v).unwrap_err();
        assert!(e.to_string().contains("idleMinute"), "{e}");
    }

    #[test]
    fn skills_from_a_mounted_folder() {
        use std::os::unix::fs::symlink;
        // a ConfigMap volume with items: the visible entries are links into ..data
        let d = std::env::temp_dir().join(format!("hermit-skills-{}", uuid::Uuid::new_v4().simple()));
        let data = d.join("..2026_11_04_00_00_00.1");
        std::fs::create_dir_all(data.join("web")).unwrap();
        std::fs::create_dir_all(data.join("labels")).unwrap();
        std::fs::write(data.join("web/SKILL.md"), "網頁\n").unwrap();
        std::fs::write(data.join("labels/SKILL.md"), "l\n").unwrap();
        std::fs::write(data.join("labels/design-format.md"), "f\n").unwrap();
        symlink("..2026_11_04_00_00_00.1", d.join("..data")).unwrap();
        symlink("..data/web", d.join("web")).unwrap();
        symlink("..data/labels", d.join("labels")).unwrap();
        let skills = read_skills(&d).unwrap();
        assert_eq!(
            skills.keys().map(String::as_str).collect::<Vec<_>>(),
            ["labels/SKILL.md", "labels/design-format.md", "web/SKILL.md"]
        );
        assert_eq!(skills["web/SKILL.md"], "網頁\n");

        let file: HermitJson = serde_json::from_str(EXAMPLE).unwrap();
        let c = Config::from_parts(file, "i", "r", "t").unwrap();
        assert!(c.managed_skills.is_empty());
        let c = c.with_skills(skills).unwrap();
        assert_eq!(c.managed_skills.len(), 3);
        let mut bad = BTreeMap::new();
        bad.insert("../x".to_owned(), String::new());
        assert!(c.clone().with_skills(bad).is_err());
        let mut big = BTreeMap::new();
        big.insert(
            "a/b".to_owned(),
            "x".repeat(hermit_api::sandbox_surface::MANAGED_MAX_BYTES),
        );
        assert!(c.with_skills(big).unwrap_err().to_string().contains("over"));

        // a folder inside a skill, or a file at the top, is refused
        std::fs::create_dir_all(data.join("web/sub")).unwrap();
        assert!(read_skills(&d).is_err());
        std::fs::remove_dir(data.join("web/sub")).unwrap();
        std::fs::write(d.join("stray.md"), "x").unwrap();
        assert!(read_skills(&d).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
