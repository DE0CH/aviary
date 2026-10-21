//! Where things are on a host. `SBX_ROOT` (tests only) prefixes every path sandboxd itself reads or writes.
use crate::fixed::Str;
use core::fmt::Write as _;

static mut ROOT: Str<200> = Str::new();

pub fn init() {
    let v = std::env::var_os("SBX_ROOT");
    // SAFETY: once at start, before other threads.
    unsafe { (*core::ptr::addr_of_mut!(ROOT)).set(v.as_ref().and_then(|s| s.to_str()).unwrap_or("")) };
}

pub fn root() -> &'static str {
    // SAFETY: read-only after init.
    unsafe { (*core::ptr::addr_of!(ROOT)).as_str() }
}

/// runsc's state root (not prefixed: runsc owns it; tests replace runsc)
pub const RUNSC_ROOT: &str = "/run/sbx/runsc";

pub type P = Str<320>;

pub fn p(args: core::fmt::Arguments) -> P {
    let mut s = Str::new();
    let _ = s.push(root());
    let _ = s.write_fmt(args);
    s
}

#[macro_export]
macro_rules! path {
    ($($a:tt)*) => { $crate::paths::p(format_args!($($a)*)) };
}

pub fn sandboxes() -> P {
    p(format_args!("/var/lib/sbx/sandboxes"))
}
pub fn bundles() -> P {
    p(format_args!("/var/lib/sbx/bundles"))
}
pub fn images() -> P {
    p(format_args!("/var/lib/sbx/images"))
}
pub fn ckpt() -> P {
    p(format_args!("/run/sbx/ckpt"))
}
pub fn logs() -> P {
    p(format_args!("/var/log/sbx"))
}
pub fn run_dir() -> P {
    p(format_args!("/run/sbx"))
}
/// the sandboxes' cgroups (runsc's cgroupsPath /sbx/<name>)
pub fn sbx_cgroup(name: &str, file: &str) -> P {
    p(format_args!("/sys/fs/cgroup/sbx/{name}/{file}"))
}

/// `base` + `suffix` (base already has the root prefix).
pub fn join(base: &str, suffix: &str) -> P {
    let mut s = Str::new();
    let _ = s.push(base);
    let _ = s.push(suffix);
    s
}
