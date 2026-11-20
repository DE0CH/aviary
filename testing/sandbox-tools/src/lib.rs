//! The sandbox image's tool list (images/sandbox/Dockerfile) and its check: every tool is run as the sandbox user
//! (uid 1000) in a login shell, as the agent's Bash tool runs it, in a container with no network. The list is the
//! test: a tool added to the image is added here first, and the check fails until the image has it.
//!
//! The pure half lives here (the list, the script the container runs, reading its answer); `main.rs` is the effect
//! (one `docker run`).

use std::fmt::Write;

/// One tool: its name in the report and the command whose success shows it works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool {
    pub name: &'static str,
    pub check: &'static str,
}

const fn t(name: &'static str, check: &'static str) -> Tool {
    Tool { name, check }
}

/// Every tool the image carries for the agent, beyond what hermit spec 17's smoke list already checks.
pub const TOOLS: &[Tool] = &[
    // shell and system
    t("tmux", "tmux -V"),
    t("openssl", "openssl version"),
    t("ripgrep", "rg --version"),
    t("nano", "nano --version"),
    t("vim", "vim --version"),
    t("gcc and make", "gcc --version && make --version"),
    t("gnupg", "gpg --version"),
    t("rsync", "rsync --version"),
    t("age", "age --version"),
    t("age-plugin-tag", "command -v age-plugin-tag"),
    t("htop", "htop --version"),
    t("sqlite3", "sqlite3 --version"),
    t("dig", "dig -v"),
    t("ping", "ping -V"),
    t("rsvg-convert", "rsvg-convert --version"),
    t("killall and fuser", "killall --version && command -v fuser"),
    t("lsof", "lsof -v"),
    t("ip", "ip -V"),
    t("sshpass", "sshpass -V"),
    t("pv", "pv --version"),
    t("zstd", "zstd --version"),
    t("whois", "whois --version"),
    t("gawk", "gawk --version"),
    t("bc", "echo 1+1 | bc"),
    t("xxd", "echo hi | xxd"),
    t("nc", "nc -h 2>&1 | grep -q OpenBSD"),
    t("brotli", "brotli --version"),
    t("uconv", "uconv --version"),
    t("heif-convert", "command -v heif-convert"),
    t("column", "echo 'a b' | column -t"),
    t("psql and pg_dump", "psql --version && pg_dump --version"),
    t("zbarimg", "zbarimg --version"),
    t("wg", "wg --version"),
    t("xdotool", "xdotool version"),
    t("xclip", "xclip -version"),
    t("imagemagick", "convert -version"),
    t("libreoffice", "soffice --version"),
    // cloud, cluster and release tools
    t("rclone", "rclone version"),
    t("kubectl", "kubectl version --client"),
    t("kubeseal", "kubeseal --version"),
    t("cloudflared", "cloudflared --version"),
    t("versitygw", "versitygw --version"),
    t("sops", "sops --version"),
    t("litestream", "litestream version"),
    t("actionlint", "actionlint --version"),
    t("crane", "crane version"),
    t("flyctl", "flyctl version"),
    t("hcloud", "hcloud version"),
    t("tailscale", "tailscale version && command -v tailscaled"),
    t("mihomo", "mihomo -v"),
    t("yt-dlp", "yt-dlp --version"),
    t("typst", "typst --version"),
    t("fastlane", "fastlane --version"),
    t("uv", "uv --version"),
    t("wrangler", "wrangler --version"),
    t("vercel", "vercel --version"),
    t("tcb", "tcb --version"),
    t("browse", "command -v browse"),
    // languages, built offline as the sandbox user
    t(
        "go",
        "d=$(mktemp -d) && cd \"$d\" && printf 'package main\\nfunc main() {}\\n' > m.go && go mod init x >/dev/null 2>&1 && go build -o x . && ./x",
    ),
    t(
        "rust",
        "d=$(mktemp -d) && cd \"$d\" && cargo new -q x && cd x && cargo build -q --offline && ./target/debug/x && rustfmt --version && cargo clippy --version",
    ),
    t("ruby", "ruby --version"),
    // browsers
    t("google-chrome", "google-chrome --version"),
    t(
        "chromium (playwright)",
        "test -x /opt/pw-browsers/chromium && chromium --version",
    ),
    t("headless shell (playwright)", "test -x /opt/pw-browsers/headless_shell"),
    t("webkit (playwright)", "ls -d /opt/pw-browsers/webkit-*"),
    t(
        "playwright for node",
        "node -e \"const p = require('playwright'); require('fs').accessSync(p.chromium.executablePath())\"",
    ),
    t(
        "playwright for python",
        "python3 -c \"import os; from playwright.sync_api import sync_playwright as s; p = s().start(); assert os.path.exists(p.chromium.executable_path); p.stop()\"",
    ),
    t("pymobiledevice3", "pymobiledevice3 version"),
    // Python libraries (the venv /opt/py)
    t(
        "python libraries",
        "python3 -c 'import requests, bs4, lxml, pymupdf, docx, pyotp, PIL, boto3, telethon, socks, python_socks, pptx, pyzbar.pyzbar, curl_cffi, eth_account, pproxy, websocket, zxingcpp, qrcode, numpy, aliyunsdkcore, yaml, huggingface_hub, gradio_client, matplotlib, msal, korail_mobile_api, edge_tts, tronpy, rapidocr, onnxruntime, asyncvnc, camoufox, psycopg, psycopg_pool, openpyxl, opencc, jieba, pillow_heif, reportlab, weasyprint, markdown, tokenizers, nacl'",
    ),
];

/// The script the container's login shell reads on stdin: one line per tool, `ok\t<name>` or `FAIL\t<name>`, each
/// check in its own subshell with its output discarded, then `done`.
#[must_use]
pub fn script(tools: &[Tool]) -> String {
    let mut s = String::new();
    for (i, tool) in tools.iter().enumerate() {
        let _ = writeln!(
            s,
            "if ( {} ) >/dev/null 2>&1 </dev/null; then echo \"ok\t{i}\"; else echo \"FAIL\t{i}\"; fi",
            tool.check
        );
    }
    s.push_str("echo done\n");
    s
}

/// What became of each tool, read from the container's output: `true` where it answered ok. A tool missing from
/// the output (the shell ended early) counts as failed, and so does every tool when `done` never came.
#[must_use]
pub fn read(tools: &[Tool], out: &str) -> Vec<(Tool, bool)> {
    let finished = out.lines().any(|l| l == "done");
    let ok: Vec<usize> = out
        .lines()
        .filter_map(|l| l.strip_prefix("ok\t"))
        .filter_map(|i| i.parse().ok())
        .collect();
    tools
        .iter()
        .enumerate()
        .map(|(i, tool)| (*tool, finished && ok.contains(&i)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_is_unique_and_every_check_is_one_line() {
        let mut names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), TOOLS.len());
        assert!(TOOLS.iter().all(|t| !t.check.contains('\n')));
    }

    #[test]
    fn a_tool_is_ok_only_when_it_answered_ok_and_the_script_finished() {
        let tools = [t("a", "true"), t("b", "false"), t("c", "true")];
        let got = read(&tools, "ok\t0\nFAIL\t1\nok\t2\ndone\n");
        assert_eq!(got.iter().map(|(_, ok)| *ok).collect::<Vec<_>>(), [true, false, true]);
        let early = read(&tools, "ok\t0\n");
        assert!(early.iter().all(|(_, ok)| !ok));
    }

    #[test]
    fn the_script_runs_each_check_in_a_subshell_and_ends_with_done() {
        let s = script(&[t("a", "x --version")]);
        assert!(s.starts_with("if ( x --version ) >/dev/null 2>&1 </dev/null; then echo \"ok\t0\""));
        assert!(s.ends_with("echo done\n"));
    }
}
