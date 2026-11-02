//! Writes the tx rule's clippy configuration (`aviary_source_rules::tx_rule`) to `<dir>/clippy.toml`:
//! `cargo run -p aviary-source-rules --bin tx-rule-conf -- target/tx-rule`, then
//! `CLIPPY_CONF_DIR=target/tx-rule cargo clippy --workspace --all-targets --locked -- -A clippy::disallowed_methods -D warnings`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> Result<ExitCode, String> {
    let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        return Err("usage: tx-rule-conf <output dir>".into());
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let metadata = aviary_source_rules::cargo_metadata(env!("CARGO"), &root)?;
    let conf = aviary_source_rules::tx_rule::tx_rule_conf(&root, &metadata).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join("clippy.toml");
    std::fs::write(&file, conf).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(ExitCode::SUCCESS)
}
