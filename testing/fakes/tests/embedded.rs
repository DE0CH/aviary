//! The fixture helpers read embedded copies (they work outside the checkout), and the embedded set is every fixture
//! on disk, byte for byte.
#![allow(clippy::unwrap_used)] // tests

use std::path::{Path, PathBuf};

fn on_disk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            on_disk(&p, root, out);
        } else if p.extension().is_some_and(|x| x == "json") {
            let rel = p.strip_prefix(root).unwrap().to_str().unwrap().replace('\\', "/");
            out.push((rel, std::fs::read_to_string(&p).unwrap()));
        }
    }
}

#[test]
fn every_fixture_is_embedded_unchanged() {
    for krate in ["magpie-api", "hermit-api", "supervisor-api"] {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates")
            .join(krate)
            .join("fixtures");
        let mut disk = Vec::new();
        on_disk(&root, &root, &mut disk);
        disk.sort();
        let mut embedded: Vec<(String, String)> = aviary_fakes::fixtures::all(krate)
            .map(|(p, t)| (p.to_owned(), t.to_owned()))
            .collect();
        embedded.sort();
        assert_ne!(disk.len(), 0, "{krate}");
        assert_eq!(embedded, disk, "{krate}");
    }
    assert_ne!(aviary_fakes::fixtures::magpie_replies("ack_command").len(), 0);
}
