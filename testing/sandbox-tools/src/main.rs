//! sandbox-tools IMAGE: runs the tool list's checks in IMAGE (ci.yml job `sandbox-tools`) and exits 1 when any tool
//! is missing or broken. One container: the image's login shell as uid 1000, no network, the sandbox's hostname (so
//! sudo resolves it, as in images/sandbox/smoke.sh), the script on stdin.

use std::io::Write;
use std::process::{Command, ExitCode, Stdio};

use aviary_sandbox_tools::{TOOLS, read, script};

fn main() -> ExitCode {
    let mut out = std::io::stdout().lock();
    let Some(image) = std::env::args().nth(1) else {
        let _ = writeln!(out, "usage: sandbox-tools IMAGE");
        return ExitCode::from(2);
    };
    let child = Command::new("docker")
        .args([
            "run",
            "--rm",
            "-i",
            "--network",
            "none",
            "--hostname",
            "sandbox",
            "--add-host",
            "sandbox:127.0.1.1",
        ])
        .args(["-u", "1000", "--entrypoint", "/bin/bash", &image, "-l", "-s"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(out, "FAIL: docker run: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(mut stdin) = child.stdin.take()
        && let Err(e) = stdin.write_all(script(TOOLS).as_bytes())
    {
        let _ = writeln!(out, "FAIL: writing the script: {e}");
    }
    let answer = match child.wait_with_output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(e) => {
            let _ = writeln!(out, "FAIL: docker run: {e}");
            return ExitCode::FAILURE;
        }
    };
    let results = read(TOOLS, &answer);
    for (tool, ok) in &results {
        let _ = writeln!(
            out,
            "{}: {} ({})",
            if *ok { "ok" } else { "FAIL" },
            tool.name,
            tool.check
        );
    }
    let failed = results.iter().filter(|(_, ok)| !ok).count();
    let _ = writeln!(out, "{} of {} tools work", results.len() - failed, results.len());
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
