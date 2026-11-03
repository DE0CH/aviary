//! `supervisor-keeper`: PID 1 of the sandbox (supervisor 4.2). It takes no arguments and starts
//! `/usr/local/bin/supervisor run` with the environment it was itself started with. The one thing it reads of that
//! environment is whether `HTTP_PROXY` carries the sandbox's proxy credential (DESIGN 12 D1): without one no
//! supervisor could ever start, so the keeper exits 78 at once with "no sandbox proxy credential" (hermit 17)
//! rather than restart a supervisor that fails the same way, which would hide the cause in a crash loop.

use supervisor::keeper::server::{Config, Keeper, log};

/// `EX_CONFIG`: the sandbox was started without what it needs.
const EXIT_NO_CREDENTIAL: i32 = 78;

fn main() {
    let credential = aviary_core::env::try_required(supervisor::boot::PROXY_VAR)
        .is_ok_and(|p| supervisor::boot::proxy_token(&p).is_some());
    if !credential {
        log(
            "no sandbox proxy credential",
            &[("exit", i64::from(EXIT_NO_CREDENTIAL))],
        );
        std::process::exit(EXIT_NO_CREDENTIAL);
    }
    let code = match Keeper::new(Config::sandbox()) {
        Ok(mut k) => k.run(),
        Err(e) => {
            log(
                "keeper start failed",
                &[("errno", i64::from(e.raw_os_error().unwrap_or(0)))],
            );
            70
        }
    };
    std::process::exit(code);
}
