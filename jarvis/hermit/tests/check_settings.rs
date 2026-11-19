//! `hermit-jarvis check-settings PATH`, the check DE0CH/deyao-ai's CI runs on its settings file with the pinned
//! image: a file this build would start on passes, one it would refuse (a model with no level of its own) fails.

#![allow(clippy::unwrap_used)]

use std::process::Command;

fn check(text: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("check-settings-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}.json", text.len()));
    std::fs::write(&path, text).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_hermit-jarvis"))
        .args(["check-settings", path.to_str().unwrap()])
        .output()
        .unwrap();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), said)
}

#[test]
fn check_settings_passes_a_file_hermit_starts_on_and_fails_one_it_refuses() {
    let file = include_str!("settings.json");
    let (ok, said) = check(file);
    assert!(ok, "{said}");

    let mut v: serde_json::Value = serde_json::from_str(file).unwrap();
    v["models"][0].as_object_mut().unwrap().remove("reasoning");
    let (ok, said) = check(&v.to_string());
    assert!(!ok, "a model with no level of its own passed: {said}");
    assert!(
        said.contains("claude-opus-5-5") && said.contains("reasoning"),
        "{said}"
    );
}
