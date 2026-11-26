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
    /// App features this deployment turns on, each off unless named.
    #[serde(default)]
    pub features: FeaturesJson,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeaturesJson {
    /// The question and wait cards in the history (magpie rule 11; `hermit_api::deployment::Features`).
    #[serde(default)]
    pub person_cards: bool,
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
    /// How many live sandboxes one person may hold at a time, beside `maxActive` for everybody together: a
    /// person's address (lowercase) to their number, `*` for everyone not named. A start over the number asks that
    /// person's own idle sandboxes to give way, and is refused as busy when none does within
    /// `capacityWaitSeconds`. Absent or empty = no limit per person.
    #[serde(default)]
    pub max_active_by_person: BTreeMap<String, u32>,
    /// Whether a start refused at a person's own number (`maxActiveByPerson`) counts as a failed start on the
    /// operator page (the verdict, the recent errors, the failed count): a person's address (lowercase) to true or
    /// false, `*` for everyone not named. Absent = true. A refusal of a person set to false is still listed with the
    /// starts, marked `quiet`; that person's other failed starts still count.
    #[serde(default)]
    pub notify_person_limit: BTreeMap<String, bool>,
    pub idle_minutes: u32,
    pub max_lifetime_hours: u32,
    pub lifetime_grace_minutes: u32,
    pub capacity_wait_seconds: u32,
    pub host_wait_seconds: u32,
    pub supervisor_wait_seconds: u32,
    pub start_deadline_seconds: u32,
    pub pool_victim_idle_seconds: u32,
    /// How long a ready sandbox may go without an activity report before the silence rule's two health checks
    /// (hermit 12 step 3), and half of it for a `PUT`'s early check. Absent = the timings' own (90 s and 45 s). A
    /// deployment whose supervisors report to another hermit (Jarvis 4's fabric hermit) sets it past any sandbox's
    /// life.
    #[serde(default)]
    pub silence_seconds: Option<u32>,
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
    /// The deployment's shared folder: folder `<drive>` at the top of the `JuiceFS` volume, mounted at `/drive` in
    /// every sandbox (one folder all conversations share; a deployment's own file page may serve the same folder).
    /// Absent = none. Lowercase letters, digits and `-`, never `ws`.
    #[serde(default)]
    pub drive: Option<String>,
    /// The deployment's other persistent folders (at most 8): each conversation gets folder `<name>/<conversation>`
    /// of the `JuiceFS` volume, with the quota named here, mounted at `path` in its sandbox, and removed with its
    /// workspace. hermit and the fabric know nothing of what a path means: the deployment lays its sandbox out.
    /// Absent or empty = none.
    #[serde(default)]
    pub mounts: Vec<MountJson>,
}

/// One of `sandbox.mounts`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MountJson {
    /// The folder at the top of the volume that holds every conversation's: lowercase letters, digits and `-`,
    /// never `ws` or the shared folder's name.
    pub name: String,
    /// Where it is mounted in the sandbox (sandboxd's rule for a spec's mount path).
    pub path: String,
    #[serde(rename = "quotaGiB")]
    pub quota_gib: u32,
    pub quota_inodes: u64,
}

/// A per-person map's names (`sandbox.maxActiveByPerson`, `sandbox.notifyPersonLimit`): a lowercase address or `*`.
fn person_names<V>(key: &str, by: &BTreeMap<String, V>) -> Result<(), ConfigError> {
    for who in by.keys() {
        let address = who.contains('@') && *who == who.trim().to_lowercase();
        if !(who == "*" || address) {
            return Err(invalid(format!(
                "sandbox.{key}: {who:?} is neither a lowercase address nor *"
            )));
        }
    }
    Ok(())
}

/// `sandbox.maxActiveByPerson`: each name a lowercase address or `*`, each number positive.
fn by_person_rules(by: &BTreeMap<String, u32>) -> Result<(), ConfigError> {
    person_names("maxActiveByPerson", by)?;
    for (who, n) in by {
        if *n == 0 {
            return Err(invalid(format!("sandbox.maxActiveByPerson: {who:?} must be positive")));
        }
    }
    Ok(())
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
    /// The one address the gateway sends the sandboxes' proxy connections to: a load balancer in front of the
    /// `fabric.egressService` pods, which balances and health-checks them. Absent: the gateway balances over the
    /// Service's ready pods itself (hermit watches its `EndpointSlices`).
    #[serde(default)]
    pub egress_target: Option<std::net::Ipv4Addr>,
    /// The hosts' sandboxes also reach the internet straight from their host's public address (sandboxd's host.json
    /// `directEgress`), beside the proxy at the egress address. Absent or false: the proxy is a sandbox's only way
    /// out. A sandbox's direct connections do not survive its move to another host (they leave from the host's
    /// address), so a deployment that turns this on keeps its sandboxes where they are (a `fabric.migration.minAgeSeconds`
    /// no sandbox reaches).
    #[serde(default)]
    pub direct_egress: bool,
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
    /// This fabric freezes sandboxes (jarvis4 docs/PLAN-fabric.md): a freeze pauses the sandbox on its host and the
    /// repacking evicts frozen ones to the bucket when their slot is needed. Absent: the freeze and thaw routes
    /// answer 501 and no row is ever `frozen` (YYC AI's fabric, which keeps release).
    #[serde(default)]
    pub freeze: Option<FreezeJson>,
}

/// The freeze settings ([`FabricJson::freeze`]).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FreezeJson {
    /// The bucket frozen images go to (in the hosts' region), with `frozen/<sandbox>/<freeze id>` keys.
    pub bucket: String,
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

/// What a host's disk holds beside the sandboxes' scratch: the system and the unpacked sandbox image (the AMI's
/// own snapshot is 16 GiB).
pub const HOST_SYSTEM_GIB: u64 = 16;

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
    /// The hosts' disk, GiB (gp3): the launch template's volume, which `fabric-aws` writes (40 when
    /// absent). It must hold every sandbox of a host at `sandbox.tmpGiB` in full, plus the system and the image.
    #[serde(default, rename = "diskGiB")]
    pub disk_gib: Option<u32>,
    /// The S3 bucket (in `region`) that holds the sandbox images as `images/<digest>.tar.zst`, which
    /// `fabric-aws publish-image` puts there: hosts fetch an image they lack from it and the gateway serves
    /// none. Absent: the gateway is the hosts' image server.
    #[serde(default)]
    pub image_bucket: Option<String>,
    /// Images of the bucket no one uses are deleted (scaler step 8, with the EC2 reconcile): every image but the
    /// configured one, those of the sandboxes that are not gone (frozen ones restore on theirs), those of the sandboxes
    /// started in the last 7 days (a replica of a rollout still on the image before) and those put there in the last
    /// day. Absent or false: nothing is ever deleted from the bucket (YYC AI). Jarvis 4's fabric (Deyao, 2026-11-20):
    /// the bucket grew by an image of about 4 GB with every pin.
    #[serde(default)]
    pub prune_images: bool,
}

impl AwsJson {
    /// Where a host fetches an image it lacks (sandboxd's `imageSource`), when the images are in a bucket.
    #[must_use]
    pub fn image_source(&self) -> Option<String> {
        self.image_bucket
            .as_ref()
            .map(|b| format!("https://{b}.s3.{}.amazonaws.com", self.region))
    }
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
    /// A chat beside this hermit (magpie and egress, whose summaries the page reads: people, View as, the budget,
    /// model errors). `false`: a hermit with no chat (Jarvis 4's fabric), whose page reads neither and shows no
    /// "could not read" for them. Absent: `true`.
    #[serde(default = "ops_chat")]
    pub chat: bool,
}

fn ops_chat() -> bool {
    true
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
            ("silenceSeconds", s.silence_seconds.unwrap_or(1)), // optional: positive when set
            ("pids", s.pids),
            ("openFiles", s.open_files),
        ] {
            if v == 0 {
                return Err(invalid(format!("sandbox.{name} must be positive")));
            }
        }
        by_person_rules(&s.max_active_by_person)?;
        person_names("notifyPersonLimit", &s.notify_person_limit)?;
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
        if s.drive.as_deref().is_some_and(|d| !valid_drive(d)) {
            return Err(invalid(
                "sandbox.drive must be lowercase letters, digits and -, and not ws",
            ));
        }
        mount_rules(&s.mounts, s.drive.as_deref())?;
        let workspace_quota_bytes = u64::from(s.workspace.quota_gib) << 30;
        // sandboxd's range for a spec's `scratchBytes`
        if !(1..=1024).contains(&s.tmp_gib) {
            return Err(invalid("sandbox.tmpGiB must be in [1, 1024]"));
        }
        let tmp_bytes = u64::from(s.tmp_gib) << 30;
        // a named disk must hold what it is for: every sandbox of a host with its scratch full, beside the system
        // and the unpacked image (`HOST_SYSTEM_GIB`); the scratch files are sparse, so nothing else would say so
        // until a sandbox's writes fail short of its size
        if let Some(disk) = file.fabric.aws.disk_gib {
            let fb = &file.fabric.aws.fallback_host;
            let free = parse_quantity(&fb.memory)
                .unwrap_or(0)
                .saturating_sub(parse_quantity(&fb.reserve).unwrap_or(0));
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // floor of a small positive value
            let by_cpu = (f64::from(fb.cpus) * f64::from(file.fabric.cpu_overcommit) / f64::from(s.cpu)).floor() as u64;
            let slots = (free / memory_bytes).min(by_cpu);
            let need = slots * u64::from(s.tmp_gib) + HOST_SYSTEM_GIB;
            if u64::from(disk) < need {
                return Err(invalid(format!(
                    "fabric.aws.diskGiB {disk} is too small: a host takes {slots} sandboxes of sandbox.tmpGiB {} \
                     and needs {HOST_SYSTEM_GIB} GiB for the system and the image, {need} GiB",
                    s.tmp_gib
                )));
            }
        }
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

/// `sandbox.mounts`: names and paths sandboxd takes (its rules for a spec's `mounts`), a quota on each.
fn mount_rules(mounts: &[MountJson], drive: Option<&str>) -> Result<(), ConfigError> {
    if mounts.len() > sandboxd_api::MOUNTS_MAX {
        return Err(invalid(format!("sandbox.mounts: at most {}", sandboxd_api::MOUNTS_MAX)));
    }
    for (i, m) in mounts.iter().enumerate() {
        if !valid_drive(&m.name) || drive == Some(m.name.as_str()) {
            return Err(invalid(format!(
                "sandbox.mounts: name {:?} must be lowercase letters, digits and -, and neither ws nor sandbox.drive",
                m.name
            )));
        }
        if !valid_mount_path(&m.path) {
            return Err(invalid(format!(
                "sandbox.mounts: path {:?} must be an absolute plain path clear of the sandbox's own mounts",
                m.path
            )));
        }
        if m.quota_gib == 0 || m.quota_inodes == 0 {
            return Err(invalid(format!(
                "sandbox.mounts: {} needs a quota (quotaGiB, quotaInodes)",
                m.name
            )));
        }
        for o in &mounts[..i] {
            if o.name == m.name || at_or_above(&o.path, &m.path) || at_or_above(&m.path, &o.path) {
                return Err(invalid(format!(
                    "sandbox.mounts: {} and {} share a name or sit on one another",
                    o.name, m.name
                )));
            }
        }
    }
    Ok(())
}

/// Is `a` the path `b` or a directory above it.
fn at_or_above(a: &str, b: &str) -> bool {
    b == a || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/'))
}

/// sandboxd's rule for a mount's path (`sandboxd` `config.rs` `valid_mount_path`): absolute and plain, not the
/// root, not on, under or above one of sandboxd's own mounts, and not on or under the supervisor's `~/.claude`.
fn valid_mount_path(p: &str) -> bool {
    const OWN: [&str; 8] = [
        "/workspace",
        "/drive",
        "/tmp",
        "/var/lib/docker",
        "/proc",
        "/dev",
        "/sys",
        "/run",
    ];
    let plain = |n: &str| {
        !n.is_empty()
            && n != "."
            && n != ".."
            && n.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || c == b'-')
    };
    p.len() <= 128
        && p.strip_prefix('/').is_some_and(|rest| rest.split('/').all(plain))
        && !OWN.iter().any(|own| at_or_above(own, p) || at_or_above(p, own))
        && !at_or_above("/home/sandbox/.claude", p)
}

/// sandboxd's rule for a spec's `drive`: a folder name, never `ws` (the conversations' workspaces).
fn valid_drive(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 64
        && d != "ws"
        && d.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
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
    fn a_persons_number_of_sandboxes_is_positive_and_named_by_a_lowercase_address_or_a_star() {
        let load = |by: serde_json::Value| {
            let mut v: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
            v["sandbox"]["maxActiveByPerson"] = by;
            Config::from_parts(serde_json::from_value(v).unwrap(), "i", "r", "t")
        };
        let c = load(serde_json::json!({"bot@example.net": 3, "*": 5})).unwrap();
        assert_eq!(c.sandbox().max_active_by_person["bot@example.net"], 3);
        assert_eq!(c.sandbox().max_active_by_person["*"], 5);
        for bad in [
            serde_json::json!({"bot@example.net": 0}),
            serde_json::json!({"Bot@example.net": 3}),
            serde_json::json!({"everyone": 3}),
        ] {
            let e = load(bad).unwrap_err().to_string();
            assert!(e.contains("maxActiveByPerson"), "{e}");
        }
        let none: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
        let c = Config::from_parts(serde_json::from_value(none).unwrap(), "i", "r", "t").unwrap();
        assert!(c.sandbox().max_active_by_person.is_empty());
    }

    #[test]
    fn notify_person_limit_names_a_lowercase_address_or_a_star_and_is_absent_by_default() {
        let load = |by: serde_json::Value| {
            let mut v: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
            v["sandbox"]["notifyPersonLimit"] = by;
            Config::from_parts(serde_json::from_value(v).unwrap(), "i", "r", "t")
        };
        let c = load(serde_json::json!({"bot@example.net": false, "*": true})).unwrap();
        assert!(!c.sandbox().notify_person_limit["bot@example.net"]);
        assert!(c.sandbox().notify_person_limit["*"]);
        for bad in [
            serde_json::json!({"Bot@example.net": false}),
            serde_json::json!({"everyone": false}),
        ] {
            let e = load(bad).unwrap_err().to_string();
            assert!(e.contains("notifyPersonLimit"), "{e}");
        }
        let none: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
        let c = Config::from_parts(serde_json::from_value(none).unwrap(), "i", "r", "t").unwrap();
        assert!(c.sandbox().notify_person_limit.is_empty());
    }

    #[test]
    fn a_named_disk_must_hold_every_sandboxs_scratch() {
        let load = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut v: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
            edit(&mut v);
            Config::from_parts(serde_json::from_value(v).unwrap(), "i", "r", "t")
        };
        // 7Gi on the fallback host (15.5Gi - 1Gi, 4 CPUs x 3): 2 sandboxes; 2 x 100 + 16 = 216 GiB
        let sized = |disk: u32| {
            load(&move |v| {
                v["sandbox"]["memory"] = "7Gi".into();
                v["sandbox"]["tmpGiB"] = 100.into();
                v["fabric"]["aws"]["fallbackHost"] =
                    serde_json::json!({"memory": "15.5Gi", "reserve": "1Gi", "cpus": 4});
                v["fabric"]["cpuOvercommit"] = 3.into();
                v["fabric"]["aws"]["diskGiB"] = disk.into();
            })
        };
        let c = sized(216).unwrap();
        assert_eq!((c.tmp_bytes, c.file.fabric.aws.disk_gib), (100 << 30, Some(216)));
        let e = sized(215).unwrap_err().to_string();
        assert!(e.contains("2 sandboxes") && e.contains("216 GiB"), "{e}");
        // no disk named: nothing to check (the template keeps fabric-aws's default)
        assert!(load(&|v| v["sandbox"]["tmpGiB"] = 100.into()).is_ok());
        // sandboxd refuses a scratch outside [1 GiB, 1 TiB]
        assert!(load(&|v| v["sandbox"]["tmpGiB"] = 0.into()).is_err());
        // the gateway's one egress target (a load balancer): none unless set, and an IPv4 address
        assert_eq!(load(&|_| {}).unwrap().file.fabric.egress_target, None);
        assert_eq!(
            load(&|v| v["fabric"]["egressTarget"] = "10.0.0.50".into())
                .unwrap()
                .file
                .fabric
                .egress_target,
            Some(std::net::Ipv4Addr::new(10, 0, 0, 50))
        );
        // the shared folder: none unless named, and a name sandboxd takes
        assert_eq!(load(&|_| {}).unwrap().file.sandbox.drive, None);
        assert_eq!(
            load(&|v| v["sandbox"]["drive"] = "drive".into())
                .unwrap()
                .file
                .sandbox
                .drive
                .as_deref(),
            Some("drive")
        );
        for no in ["ws", "", "a/b", "..", "Drive"] {
            assert!(load(&move |v| v["sandbox"]["drive"] = no.into()).is_err(), "{no}");
        }
        assert!(load(&|v| v["sandbox"]["tmpGiB"] = 1025.into()).is_err());
    }

    /// hermit.json with `edit` applied to the example, read as [`Config::load`] reads it: a member hermit.json does
    /// not have is a parse error (`deny_unknown_fields`), returned and not a panic, so a case can expect it.
    fn load_edited(edit: &dyn Fn(&mut serde_json::Value)) -> Result<Config, ConfigError> {
        let mut v: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
        edit(&mut v);
        let file = serde_json::from_value(v).map_err(|e| ConfigError::Parse(e.to_string()))?;
        Config::from_parts(file, "i", "r", "t")
    }

    #[test]
    fn direct_egress_and_mounts_are_off_unless_named() {
        let load = load_edited;
        // direct egress: off unless named, and then a host's user data says so (sandboxd's host.json
        // `directEgress`); a deployment that leaves it off sends the user data it always sent
        let secrets = crate::fabric::gvisor::HostSecrets {
            ca: String::new(),
            cert: String::new(),
            key: String::new(),
            juicefs_meta_url: String::new(),
            juicefs_meta_password: String::new(),
        };
        let host = |c: &Config| {
            let ip = std::net::IpAddr::from([10, 99, 0, 10]);
            crate::fabric::scaler::user_data(c, "h", ip, "priv", "pub", &secrets)["host"].clone()
        };
        let off = load(&|_| {}).unwrap();
        assert!(!off.file.fabric.direct_egress && host(&off).get("directEgress").is_none());
        let on = load(&|v| v["fabric"]["directEgress"] = true.into()).unwrap();
        assert_eq!(host(&on)["directEgress"], true);
        let named_off = load(&|v| v["fabric"]["directEgress"] = false.into()).unwrap();
        assert_eq!(host(&named_off), host(&off));
        // the other persistent folders: none unless named; each a per-conversation folder with its own quota
        assert!(load(&|_| {}).unwrap().file.sandbox.mounts.is_empty());
        let home =
            serde_json::json!({"name": "home", "path": "/home/claude", "quotaGiB": 20, "quotaInodes": 2_000_000});
        let h = home.clone();
        let c = load(&move |v| v["sandbox"]["mounts"] = serde_json::json!([h])).unwrap();
        let m = &c.file.sandbox.mounts[0];
        assert_eq!(
            (m.name.as_str(), m.path.as_str(), m.quota_gib, m.quota_inodes),
            ("home", "/home/claude", 20, 2_000_000)
        );
        let bad: [(&str, serde_json::Value); 14] = [
            ("name", "ws".into()),
            ("name", "".into()),
            ("name", "a/b".into()),
            ("name", "Home".into()),
            ("path", "home".into()),
            ("path", "/".into()),
            ("path", "/workspace".into()),
            ("path", "/workspace/x".into()),
            ("path", "/var/lib".into()),
            ("path", "/tmp/x".into()),
            ("path", "/home/../etc".into()),
            ("path", "/home/sandbox/.claude".into()),
            ("quotaGiB", 0.into()),
            ("quotaInodes", 0.into()),
        ];
        for (k, no) in bad {
            let mut m = home.clone();
            m[k] = no.clone();
            assert!(
                load(&move |v| v["sandbox"]["mounts"] = serde_json::json!([m])).is_err(),
                "{k}: {no}"
            );
        }
        // an unknown member is refused like everywhere in hermit.json
        let mut extra = home.clone();
        extra["mode"] = "ro".into();
        assert!(load(&move |v| v["sandbox"]["mounts"] = serde_json::json!([extra])).is_err());
        // never the shared folder's name, never two with one name, never one path on or inside another, at most 8
        let h = home.clone();
        assert!(
            load(&move |v| {
                v["sandbox"]["drive"] = "home".into();
                v["sandbox"]["mounts"] = serde_json::json!([h]);
            })
            .is_err()
        );
        let two = |name: &str, path: &str| {
            let (mut other, h) = (home.clone(), home.clone());
            other["name"] = name.into();
            other["path"] = path.into();
            load(&move |v| v["sandbox"]["mounts"] = serde_json::json!([h, other]))
        };
        assert!(two("home", "/home/other").is_err());
        assert!(two("other", "/home/claude").is_err());
        assert!(two("other", "/home/claude/x").is_err());
        assert!(two("other", "/home").is_err());
        assert_eq!(two("other", "/home/claude2").unwrap().file.sandbox.mounts.len(), 2);
        let many = |n: usize| {
            let list: Vec<serde_json::Value> = (0..n)
                .map(|i| serde_json::json!({"name": format!("m{i}"), "path": format!("/m/p{i}"), "quotaGiB": 1, "quotaInodes": 1}))
                .collect();
            load(&move |v| v["sandbox"]["mounts"] = serde_json::json!(list))
        };
        assert!(many(8).is_ok() && many(9).is_err());
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
        // a deployment's prompt and skills together run to tens of KiB (YYC's were 24 bytes under a 40 KiB bound)
        let mut roomy = BTreeMap::new();
        roomy.insert("a/b".to_owned(), "x".repeat(100 * 1024));
        assert!(c.clone().with_skills(roomy).is_ok());
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

    #[test]
    fn app_features_are_off_unless_named_and_reach_the_deployment_facts() {
        // the question and wait cards (magpie rule 11): YYC AI turns them on in its own hermit.json (Deyao,
        // 2026-11-20); pigeon reads them from the deployment facts (`GET /api/jarvis/me` `personCards`)
        let off = load_edited(&|_| {}).unwrap();
        assert!(!crate::deployment::of(&off).features.person_cards);
        let on = load_edited(&|v| v["features"] = serde_json::json!({ "personCards": true })).unwrap();
        assert!(crate::deployment::of(&on).features.person_cards);
        assert!(load_edited(&|v| v["features"] = serde_json::json!({ "cards": true })).is_err());
    }
}
