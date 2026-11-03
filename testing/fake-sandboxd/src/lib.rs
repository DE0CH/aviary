//! `fake-sandboxd` (platform 7.2): hermit's YYC fabric against Kubernetes instead of AWS and gVisor, for the
//! end-to-end harness (`testing/e2e`). Two commands, one image (`aviary-testing`):
//!
//! - `fake-sandboxd host --config FILE`: one fabric host. It serves sandboxd's API ([`sandboxd_api::router`]) on
//!   :7443 with mutual TLS from the host's own config (the user data hermit gave its instance, or the gateway's file
//!   in the harness), and runs each sandbox as a Pod of the sandbox image: Pod `<name>` with the address hermit chose
//!   (Calico's `cni.projectcalico.org/ipAddrs`), the spec's env, CPU and memory as limits, `/workspace` from the
//!   host's `JuiceFS` mount (`ws/<id>`), privileged (FUSE, Docker). Migration routes answer 501 `not_supported`.
//! - `fake-sandboxd ec2 …`: the three EC2 calls hermit makes (`RunInstances`, `DescribeInstances`,
//!   `TerminateInstances`, EC2's query protocol) on HTTPS, reached under EC2's own name through the cluster DNS. A
//!   `RunInstances` starts a host Pod (`fake-sandboxd host`) at the user data's overlay address with the user data as
//!   its config; the instance is that Pod.
//!
//! Both need a service account that may manage Pods (and, for `ec2`, Secrets) in the namespace.

pub mod ec2;
pub mod host;
pub mod tls;

/// The label every Pod this crate makes carries: `host-<name>` for a fabric host, the host's name for a sandbox.
pub const HOST_LABEL: &str = "fake-sandboxd/host";
/// Calico's annotation for a Pod's fixed address.
pub const IP_ANNOTATION: &str = "cni.projectcalico.org/ipAddrs";

/// Now as sandboxd writes it (ISO 8601, ms, `Z`).
#[must_use]
pub fn iso_now() -> String {
    iso(time::OffsetDateTime::now_utc())
}

/// A time as sandboxd writes it.
#[must_use]
pub fn iso(t: time::OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}
