//! The commands the harness runs (docker, k3d, kubectl, juicefs, openssl, sudo): each one's exit status is read,
//! each has a bound, and a network request (a download, a pull) is tried again when it fails.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// A command and what it is for (its errors name it).
pub struct Cmd {
    inner: Command,
    what: String,
    stdin: Option<Vec<u8>>,
}

/// `program args…`.
pub fn cmd(program: &str, args: &[&str]) -> Cmd {
    let mut c = Command::new(program);
    c.args(args);
    Cmd {
        inner: c,
        what: format!("{program} {}", args.join(" ")),
        stdin: None,
    }
}

/// `kubectl args…` on the harness's cluster.
pub fn kubectl(kubeconfig: &Path, args: &[&str]) -> Cmd {
    cmd("kubectl", args).env("KUBECONFIG", &kubeconfig.to_string_lossy())
}

impl Cmd {
    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.inner.env(k, v);
        self
    }

    pub fn arg(mut self, a: &str) -> Self {
        self.what.push(' ');
        self.what.push_str(a);
        self.inner.arg(a);
        self
    }

    pub fn args(mut self, a: &[&str]) -> Self {
        for x in a {
            self = self.arg(x);
        }
        self
    }

    pub fn stdin(mut self, b: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(b.into());
        self
    }

    /// Runs it, at most `secs`; its output whatever its exit status.
    pub fn output_within(mut self, secs: u64) -> Result<Output> {
        self.inner
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = self.inner.spawn().with_context(|| format!("starting {}", self.what))?;
        if let Some(b) = self.stdin.take() {
            let mut si = child.stdin.take().context("stdin")?;
            std::thread::spawn(move || {
                let _ = si.write_all(&b);
            });
        }
        let out_pipe = child.stdout.take().context("stdout")?;
        let err_pipe = child.stderr.take().context("stderr")?;
        let out = std::thread::spawn(move || read_all(out_pipe));
        let err = std::thread::spawn(move || read_all(err_pipe));
        let until = Instant::now() + Duration::from_secs(secs);
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if Instant::now() >= until {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{}: no end within {secs} s", self.what);
            }
            sleep(Duration::from_millis(50));
        };
        Ok(Output {
            status,
            stdout: out.join().unwrap_or_default(),
            stderr: err.join().unwrap_or_default(),
        })
    }

    /// Runs it, at most `secs`, which must succeed; its standard output.
    pub fn run(self, secs: u64) -> Result<String> {
        let what = self.what.clone();
        let o = self.output_within(secs)?;
        if !o.status.success() {
            bail!(
                "{what}: {}\n{}{}",
                o.status,
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }

    /// Whether it succeeds within `secs`.
    pub fn ok(self, secs: u64) -> bool {
        self.output_within(secs).is_ok_and(|o| o.status.success())
    }
}

/// `from`'s standard output streamed into `into`'s standard input, both at most `secs`; both must succeed.
pub fn pipe(from: Cmd, into: Cmd, secs: u64) -> Result<()> {
    let what = format!("{} | {}", from.what, into.what);
    let (mut a, mut b) = (from.inner, into.inner);
    let mut first = a
        .stdout(Stdio::piped())
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let out = first.stdout.take().context("stdout")?;
    let mut second = b
        .stdin(Stdio::from(out))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let e1 = first.stderr.take().context("stderr")?;
    let e2 = second.stderr.take().context("stderr")?;
    let e1 = std::thread::spawn(move || read_all(e1));
    let e2 = std::thread::spawn(move || read_all(e2));
    let until = Instant::now() + Duration::from_secs(secs);
    let (mut s1, mut s2) = (None, None);
    while s1.is_none() || s2.is_none() {
        if s1.is_none() {
            s1 = first.try_wait()?;
        }
        if s2.is_none() {
            s2 = second.try_wait()?;
        }
        if Instant::now() >= until {
            let _ = first.kill();
            let _ = second.kill();
            let _ = first.wait();
            let _ = second.wait();
            bail!("{what}: no end within {secs} s");
        }
        sleep(Duration::from_millis(50));
    }
    let (s1, s2) = (s1.context("status")?, s2.context("status")?);
    if !s1.success() || !s2.success() {
        bail!(
            "{what}: {s1}, {s2}\n{}{}",
            String::from_utf8_lossy(&e1.join().unwrap_or_default()),
            String::from_utf8_lossy(&e2.join().unwrap_or_default())
        );
    }
    Ok(())
}

fn read_all(mut r: impl std::io::Read) -> Vec<u8> {
    let mut v = Vec::new();
    let _ = r.read_to_end(&mut v);
    v
}

/// `f` until it succeeds, at most 5 tries 3 s apart: a network request (a download, a pull) is tried again.
pub fn network<T>(what: &str, mut f: impl FnMut() -> Result<T>) -> Result<T> {
    let mut tries = 0;
    loop {
        tries += 1;
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if tries < 5 => {
                eprintln!("e2e: {what} failed (try {tries} of 5): {e:#}");
                sleep(Duration::from_secs(3));
            }
            Err(e) => return Err(e.context(format!("{what}: 5 tries"))),
        }
    }
}

/// Waits (at most `secs`) until `f` holds, looking every second.
pub fn wait_for(secs: u64, what: &str, mut f: impl FnMut() -> bool) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(secs);
    while !f() {
        if Instant::now() >= until {
            bail!("timed out waiting for {what}");
        }
        sleep(Duration::from_secs(1));
    }
    Ok(())
}

/// `n` random bytes.
pub fn random(n: usize) -> Result<Vec<u8>> {
    let mut b = vec![0u8; n];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut b)?;
    Ok(b)
}

/// `n` random bytes as hex.
pub fn hex(n: usize) -> Result<String> {
    Ok(random(n)?.iter().fold(String::new(), |mut s, b| {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
        s
    }))
}

/// Writes `data` to `path` readable by its owner only.
pub fn write_private(path: &Path, data: impl AsRef<[u8]>) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| path.display().to_string())?;
    f.write_all(data.as_ref())?;
    Ok(())
}

pub fn log(s: &str) {
    eprintln!("e2e: {s}");
}
