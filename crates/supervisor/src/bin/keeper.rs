//! `supervisor-keeper`: PID 1 of the sandbox (supervisor 4.2). It takes no arguments and reads no environment: it
//! starts `/usr/local/bin/supervisor run` with the environment it was itself started with.

use supervisor::keeper::server::{Config, Keeper, log};

fn main() {
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
