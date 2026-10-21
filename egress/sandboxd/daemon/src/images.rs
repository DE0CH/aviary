//! The sandbox image on a host: unpacked once per digest into /var/lib/sbx/images/<digest>/{rootfs,config.json};
//! every sandbox shares it read-only. Pulled with crane from the registry (a hub, or any host with
//! `registryConfigDir`), else fetched unpacked from `imageSource` (an image server, e.g. the old hub).
//! Data never passes through sandboxd: `crane export | tar -x` and `curl | tar -x` are piped child to child.
use crate::config::HostConfig;
use crate::fixed::Str;
use crate::json::{self, Val};
use crate::proc::{pipeline, run_env};
use crate::util::{Error, Log, Res, each_dir_entry, exists, mono_ms, read_file};
use crate::{err, path, paths};
use core::fmt::Write as _;
use std::sync::{Condvar, Mutex};

pub fn valid_digest(d: &str) -> bool {
    d.len() == 64 && d.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn have_image(digest: &str) -> bool {
    exists(path!("/var/lib/sbx/images/{digest}/.done").as_str())
}

/// Calls `f` with each unpacked image's digest.
pub fn each_image(mut f: impl FnMut(&str)) {
    let _ = each_dir_entry(paths::images().as_str(), |d| {
        if valid_digest(d) && have_image(d) {
            f(d)
        }
    });
}

/// The image config's Env, as (key, value) pairs, from config.json read into `buf`.
pub fn image_env(digest: &str, buf: &mut [u8], mut f: impl FnMut(&str)) -> Res<()> {
    let text = read_file(path!("/var/lib/sbx/images/{digest}/config.json").as_str(), buf)
        .map_err(|e| err!(500, "image config: {}", crate::util::io_msg(&e)))?;
    let Val::Obj(o) = json::parse(text).map_err(|_| Error::new(500, "image config is not JSON"))? else {
        return Ok(());
    };
    json::object::<Error>(o, |k, v| {
        if !k.is("config") {
            return Ok(());
        }
        let Val::Obj(c) = v else { return Ok(()) };
        json::object::<Error>(c, |k, v| {
            if !k.is("Env") {
                return Ok(());
            }
            let Val::Arr(a) = v else { return Ok(()) };
            json::array::<Error>(a, |e| {
                let mut tmp = [0u8; 8192];
                if let Some(s) = e.str().and_then(|s| s.decode(&mut tmp)) {
                    f(s);
                }
                Ok(())
            })
        })
    })
}

/// Per-digest serialisation of pulls, in a fixed table (more different digests at once than slots: 503).
pub struct Pulls {
    slots: Mutex<[(Str<64>, u32); 4]>,
    cv: Condvar,
}

impl Default for Pulls {
    fn default() -> Self {
        Self::new()
    }
}

impl Pulls {
    pub fn new() -> Pulls {
        Pulls { slots: Mutex::new([(Str::new(), 0); 4]), cv: Condvar::new() }
    }

    fn acquire(&self, d: &str) -> Res<usize> {
        let mut s = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        let deadline = mono_ms() + 20 * 60_000;
        loop {
            // someone pulls this digest: wait for them
            if let Some(i) = s.iter().position(|(n, _)| n.as_str() == d) {
                if mono_ms() > deadline {
                    return Err(Error::new(503, "image pull is taking too long"));
                }
                s = self
                    .cv
                    .wait_timeout(s, std::time::Duration::from_secs(1))
                    .map(|r| r.0)
                    .unwrap_or_else(|p| p.into_inner().0);
                let _ = i;
                continue;
            }
            if let Some(i) = s.iter().position(|(n, _)| n.is_empty()) {
                s[i].0 = Str::lossy(d);
                return Ok(i);
            }
            return Err(Error::new(503, "too many image pulls at once"));
        }
    }

    fn release(&self, i: usize) {
        let mut s = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        s[i].0.clear();
        self.cv.notify_all();
    }
}

fn from_registry(cfg: &HostConfig) -> bool {
    cfg.hub || !cfg.registry_config_dir.is_empty()
}

/// Makes sure the image is unpacked here; returns its digest. With registry access, resolves `image_ref`.
pub fn ensure_image(cfg: &HostConfig, pulls: &Pulls, digest: Option<&str>, image_ref: Option<&str>) -> Res<Str<64>> {
    let mut envbuf: Str<300> = Str::new();
    if !cfg.registry_config_dir.is_empty() {
        let _ = write!(envbuf, "DOCKER_CONFIG={}", cfg.registry_config_dir);
    }
    let env_arr = [envbuf.as_str()];
    let env: &[&str] = if envbuf.is_empty() { &[] } else { &env_arr };
    let d: Str<64> = match digest {
        Some(d) => Str::lossy(d),
        None => {
            let r = image_ref
                .filter(|_| from_registry(cfg))
                .ok_or_else(|| Error::new(500, "image: need a digest (or a ref, with registry access)"))?;
            let mut out = [0u8; 256];
            let argv = ["crane", "digest", r];
            let o = run_env(&argv, env, &mut out, 60_000)?;
            if o.code != 0 {
                return Err(o.failure(&argv));
            }
            Str::lossy(o.out_str().trim().trim_start_matches("sha256:"))
        }
    };
    if !valid_digest(d.as_str()) {
        return Err(Error::new(500, "image: bad digest"));
    }
    if have_image(d.as_str()) {
        return Ok(d);
    }
    let slot = pulls.acquire(d.as_str())?;
    let r = unpack(cfg, env, d.as_str(), image_ref);
    pulls.release(slot);
    r.map(|_| d)
}

fn unpack(cfg: &HostConfig, env: &[&str], d: &str, image_ref: Option<&str>) -> Res<()> {
    if have_image(d) {
        return Ok(());
    }
    let tmp = path!("/var/lib/sbx/images/.tmp-{d}-{}", std::process::id());
    let rootfs = crate::paths::join(tmp.as_str(), "/rootfs");
    crate::proc::quiet(&["rm", "-rf", tmp.as_str()]);
    crate::util::mkdir(rootfs.as_str(), 0o755)?;
    let t0 = mono_ms();
    let r = (|| -> Res<()> {
        if from_registry(cfg) {
            let base = image_ref.map(|r| r.split([':', '@']).next().unwrap_or("")).filter(|b| !b.is_empty());
            let mut src: Str<400> = Str::new();
            let _ = write!(src, "{}@sha256:{d}", base.unwrap_or("ghcr.io/de0ch/jarvis3-sandbox"));
            // crane's `export` has a registry name with a colon only for a port: split at the first ':' or '@' as the
            // old code did ("ghcr.io/x:tag" -> "ghcr.io/x")
            pipeline(&["crane", "export", src.as_str(), "-"], env, &["tar", "-x", "-C", rootfs.as_str()], 30 * 60_000)?;
            let cfgfile = crate::paths::join(tmp.as_str(), "/config.json");
            let f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(cfgfile.as_str())?;
            use std::os::fd::AsRawFd;
            let c = crate::proc::spawn(
                &["crane", "config", src.as_str()],
                env,
                crate::proc::Io::Null,
                crate::proc::Io::Fd(f.as_raw_fd()),
                crate::proc::Io::Pipe,
            )?;
            let mut none = [0u8; 0];
            let o = crate::proc::collect(&c, &mut none, 60_000);
            if o.code != 0 {
                return Err(o.failure(&["crane", "config", src.as_str()]));
            }
        } else {
            let mut url: Str<400> = Str::new();
            if cfg.image_source.is_empty() {
                let _ = write!(url, "http://10.201.0.1:{}/images/{d}.tar.zst", cfg.image_port);
            } else {
                let _ = write!(url, "{}/images/{d}.tar.zst", cfg.image_source);
            }
            pipeline(
                &["curl", "-fsS", "--max-time", "1800", url.as_str()],
                &[],
                &["tar", "--zstd", "-x", "-C", tmp.as_str()],
                31 * 60_000,
            )?;
        }
        std::fs::File::create(crate::paths::join(tmp.as_str(), "/.done").as_str())?;
        let dst = path!("/var/lib/sbx/images/{d}");
        crate::proc::quiet(&["rm", "-rf", dst.as_str()]);
        std::fs::rename(tmp.as_str(), dst.as_str())?;
        Ok(())
    })();
    match r {
        Ok(()) => {
            Log::info("image ready").s("digest", d).n("ms", mono_ms() - t0).emit();
            Ok(())
        }
        Err(e) => {
            crate::proc::quiet(&["rm", "-rf", tmp.as_str()]);
            Err(e)
        }
    }
}
