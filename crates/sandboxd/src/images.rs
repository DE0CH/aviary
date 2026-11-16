//! The sandbox image on a host: unpacked once per digest into /var/lib/sbx/images/<digest>/{rootfs,config.json};
//! every sandbox shares it read-only. Pulled with crane from the registry (a hub, or any host with
//! `registryConfigDir`), else fetched unpacked from `imageSource` (an image server: the gateway's).
//! Data never passes through sandboxd: `crane export | tar -x` and `curl | tar -x` are piped child to child.
//! A hub's store is a cache: once it holds the image it was asked for, it keeps that one and the one before it and
//! removes the rest ([`prune`]).
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
            f(d);
        }
    });
}

/// A list of the image config (`config.Env`, `config.Entrypoint`, `config.Cmd`); never 0 as a byte (a NUL separates
/// the copies `Sandboxes::prepare` makes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ConfigList {
    Env = 1,
    Entrypoint = 2,
    Cmd = 3,
}

/// The image config's lists: `f(list, item)` for each string of `config.Env`, `config.Entrypoint` and `config.Cmd`
/// (in the file's order; a missing or null list has none), from config.json read into `buf`.
pub fn image_config(digest: &str, buf: &mut [u8], f: impl FnMut(ConfigList, &str)) -> Res<()> {
    let text = read_file(path!("/var/lib/sbx/images/{digest}/config.json").as_str(), buf)
        .map_err(|e| err!(500, "image config: {}", crate::util::io_msg(&e)))?;
    config_lists(text, f)
}

fn config_lists(text: &[u8], mut f: impl FnMut(ConfigList, &str)) -> Res<()> {
    let Val::Obj(o) = json::parse(text).map_err(|_| Error::new(500, "image config is not JSON"))? else {
        return Ok(());
    };
    json::object::<Error>(o, |k, v| {
        if !k.is("config") {
            return Ok(());
        }
        let Val::Obj(c) = v else { return Ok(()) };
        json::object::<Error>(c, |k, v| {
            let list = if k.is("Env") {
                ConfigList::Env
            } else if k.is("Entrypoint") {
                ConfigList::Entrypoint
            } else if k.is("Cmd") {
                ConfigList::Cmd
            } else {
                return Ok(());
            };
            let Val::Arr(a) = v else { return Ok(()) };
            json::array::<Error>(a, |e| {
                let mut tmp = [0u8; 8192];
                // an item that does not decode is refused, never skipped: a skipped Entrypoint or Cmd item would
                // start the sandbox with another argv, a skipped Env item without that variable
                let Some(s) = e.str().and_then(|s| s.decode(&mut tmp)) else {
                    return Err(Error::new(400, "image config item too large or not a string (Env, Entrypoint, Cmd)"));
                };
                f(list, s);
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
        let mut s = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
                    .map_or_else(|p| p.into_inner().0, |r| r.0);
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
        let mut s = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        s[i].0.clear();
        self.cv.notify_all();
    }
}

/// The newest image in `dir` other than `keep`, by the time it was unpacked (its `.done`).
fn newest_other(dir: &str, keep: &str) -> Str<64> {
    let mut newest: (Str<64>, Option<std::time::SystemTime>) = (Str::new(), None);
    let _ = each_dir_entry(dir, |d| {
        if !valid_digest(d) || d == keep {
            return;
        }
        let done = crate::paths::join(crate::paths::join(dir, "/").as_str(), d);
        let at =
            std::fs::metadata(crate::paths::join(done.as_str(), "/.done").as_str()).and_then(|m| m.modified()).ok();
        if at.is_some() && at > newest.1 {
            newest = (Str::lossy(d), at);
        }
    });
    newest.0
}

/// A hub's image store is a cache in a space with a size (the gateway pod's scratch space): once the hub holds
/// `keep`, the image hosts are sent to, every other image goes except the newest of them, the image before it, which
/// new sandboxes still use until the hub holds a new one (hermit 13.14). Returns how many it removed. A hub that
/// kept every image it ever pulled filled its node's disk (2026-11-16). Names that are not a digest (a pull under
/// way, `.tmp-…`) are left alone.
pub fn prune(dir: &str, keep: &str) -> usize {
    let before = newest_other(dir, keep);
    let mut removed = 0;
    // one at a time: nothing is removed while the directory is being read
    loop {
        let mut victim: Str<64> = Str::new();
        let _ = each_dir_entry(dir, |d| {
            if victim.is_empty() && valid_digest(d) && d != keep && d != before.as_str() {
                victim = Str::lossy(d);
            }
        });
        if victim.is_empty() {
            return removed;
        }
        let path = crate::paths::join(crate::paths::join(dir, "/").as_str(), victim.as_str());
        crate::proc::quiet(&["rm", "-rf", path.as_str()]);
        if exists(path.as_str()) {
            // it would be found again for ever
            Log::info("image not removed").s("digest", victim.as_str()).emit();
            return removed;
        }
        Log::info("image removed").s("digest", victim.as_str()).s("kept", keep).emit();
        removed += 1;
    }
}

fn from_registry(cfg: &HostConfig) -> bool {
    cfg.pulls_from_registry()
}

/// Makes sure the image is unpacked here; returns its digest. With registry access, resolves `image_ref`.
pub fn ensure_image(cfg: &HostConfig, pulls: &Pulls, digest: Option<&str>, image_ref: Option<&str>) -> Res<Str<64>> {
    let mut envbuf: Str<300> = Str::new();
    if !cfg.registry_config_dir.is_empty() {
        let _ = write!(envbuf, "DOCKER_CONFIG={}", cfg.registry_config_dir);
    }
    let env_arr = [envbuf.as_str()];
    let env: &[&str] = if envbuf.is_empty() { &[] } else { &env_arr };
    let d: Str<64> = if let Some(d) = digest {
        Str::lossy(d)
    } else {
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
    };
    if !valid_digest(d.as_str()) {
        return Err(Error::new(500, "image: bad digest"));
    }
    if !have_image(d.as_str()) {
        let slot = pulls.acquire(d.as_str())?;
        let r = unpack(cfg, env, d.as_str(), image_ref);
        pulls.release(slot);
        r?;
    }
    if cfg.gateway {
        prune(paths::images().as_str(), d.as_str());
    }
    Ok(d)
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
            let base = base.ok_or_else(|| {
                Error::new(400, "image: a pull from the registry needs a reference (ref or imageRef)")
            })?;
            let mut src: Str<400> = Str::new();
            let _ = write!(src, "{base}@sha256:{d}");
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
            // imageSource is required without registry access (HostConfig::parse)
            let mut url: Str<400> = Str::new();
            let _ = write!(url, "{}/images/{d}.tar.zst", cfg.image_source);
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

#[cfg(test)]
mod prune_tests {
    use super::*;

    #[test]
    fn a_hub_keeps_the_image_it_was_asked_for_and_the_one_before() {
        let dir = std::env::temp_dir().join(format!("sbx-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let digest = |c: char| c.to_string().repeat(64);
        // unpacked an hour apart: a (oldest), b, c, d (newest)
        for (i, c) in ['a', 'b', 'c', 'd'].into_iter().enumerate() {
            let image = dir.join(digest(c));
            std::fs::create_dir_all(image.join("rootfs")).unwrap();
            std::fs::write(image.join("rootfs/file"), b"x").unwrap();
            let done = std::fs::File::create(image.join(".done")).unwrap();
            let at =
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + 3600 * i as u64);
            done.set_modified(at).unwrap();
        }
        // a pull under way, and something that is no image
        std::fs::create_dir_all(dir.join(format!(".tmp-{}-1", digest('e')))).unwrap();
        std::fs::create_dir_all(dir.join("lost+found")).unwrap();
        let names = || {
            let mut v: Vec<String> =
                std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
            v.sort();
            v
        };
        // asked for b: the newest other image (d) stays with it, a and c go
        assert_eq!(prune(dir.to_str().unwrap(), &digest('b')), 2);
        assert_eq!(
            names(),
            vec![format!(".tmp-{}-1", digest('e')), digest('b'), digest('d'), "lost+found".to_string()]
        );
        // again: nothing more to remove
        assert_eq!(prune(dir.to_str().unwrap(), &digest('b')), 0);
        // asked for an image that is the only one: nothing goes
        assert_eq!(prune(dir.to_str().unwrap(), &digest('d')), 0);
        assert_eq!(names().len(), 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    fn lists(text: &str) -> Vec<(ConfigList, String)> {
        let mut v = Vec::new();
        config_lists(text.as_bytes(), |l, s| v.push((l, s.to_owned()))).ok().unwrap();
        v
    }

    #[test]
    fn env_entrypoint_and_cmd() {
        use ConfigList::{Cmd, Entrypoint, Env};
        let v = lists(
            r#"{"architecture":"amd64","config":{"Cmd":["--serve"],"Env":["PATH=/usr/bin","A=b"],"Entrypoint":["/usr/bin/tini","--","/opt/keeper"],"WorkingDir":"/"}}"#,
        );
        let got: Vec<(ConfigList, &str)> = v.iter().map(|(l, s)| (*l, s.as_str())).collect();
        assert_eq!(
            got,
            [
                (Cmd, "--serve"),
                (Env, "PATH=/usr/bin"),
                (Env, "A=b"),
                (Entrypoint, "/usr/bin/tini"),
                (Entrypoint, "--"),
                (Entrypoint, "/opt/keeper")
            ]
        );
        assert_eq!(lists(r#"{"config":{"Entrypoint":null,"Cmd":null}}"#), []);
        assert_eq!(lists(r#"{"rootfs":{}}"#), []);
    }

    #[test]
    fn an_item_that_does_not_decode_is_refused() {
        let long = "x".repeat(9000);
        for list in ["Entrypoint", "Cmd", "Env"] {
            let text = format!(r#"{{"config":{{"{list}":["/bin/a","{long}"]}}}}"#);
            let e = config_lists(text.as_bytes(), |_, _| {}).err().unwrap();
            assert_eq!(e.status, 400, "{list}");
            let text = format!(r#"{{"config":{{"{list}":["/bin/a",7]}}}}"#);
            assert_eq!(config_lists(text.as_bytes(), |_, _| {}).err().unwrap().status, 400, "{list}");
        }
    }
}
