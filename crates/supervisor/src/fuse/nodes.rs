//! Node ids (supervisor 9.8): one per path, assigned on first LOOKUP/CREATE/MKDIR, moved with RENAME, kept after
//! UNLINK until the next start, never reused. Persisted in `nodes.log` (append-only `assign <id> <path>`, `rename
//! <id> <path>`, `drop <id>`; compacted at start, with `next <id>`) because the sentry still holds the old ids when
//! a supervisor restarts.

#![allow(clippy::many_single_char_names, clippy::similar_names)] // FUSE argument names: h(eader), b(ody), p(ath)

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// The root's id (FUSE's).
pub const ROOT: u64 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Rec {
    path: String,
    gone: bool,
}

/// The node table.
#[derive(Debug)]
pub struct Nodes {
    by_id: HashMap<u64, Rec>,
    by_path: HashMap<String, u64>,
    next: u64,
    log: Option<File>,
}

fn esc(p: &str) -> String {
    p.replace('%', "%25").replace('\n', "%0A")
}

fn unesc(p: &str) -> String {
    p.replace("%0A", "\n").replace("%25", "%")
}

impl Nodes {
    /// A table that persists nothing (tests).
    #[must_use]
    pub fn in_memory() -> Self {
        let mut n = Self {
            by_id: HashMap::new(),
            by_path: HashMap::new(),
            next: ROOT + 1,
            log: None,
        };
        n.by_id.insert(
            ROOT,
            Rec {
                path: String::new(),
                gone: false,
            },
        );
        n.by_path.insert(String::new(), ROOT);
        n
    }

    /// Loads `path` (if present), compacts it (temporary file, `fsync`, `rename`) and appends from then on. Ids of
    /// paths unlinked before this start are gone; `next` survives, so no id is reused.
    ///
    /// # Errors
    /// The log cannot be written.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut n = Self::in_memory();
        if let Ok(f) = File::open(path) {
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                n.apply_line(&line);
            }
        }
        n.by_id.retain(|_, r| !r.gone);
        let tmp = path.with_extension("tmp");
        {
            let mut f = File::create(&tmp)?;
            let mut out = format!("next {}\n", n.next);
            let mut live: Vec<(&u64, &Rec)> = n.by_id.iter().filter(|(i, _)| **i != ROOT).collect();
            live.sort_by_key(|(i, _)| **i);
            for (i, r) in live {
                let _ = writeln!(out, "assign {i} {}", esc(&r.path));
            }
            f.write_all(out.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        n.log = Some(OpenOptions::new().append(true).open(path)?);
        Ok(n)
    }

    fn apply_line(&mut self, line: &str) {
        let mut it = line.splitn(3, ' ');
        let kind = it.next().unwrap_or("");
        let id: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let path = unesc(it.next().unwrap_or(""));
        match kind {
            "next" => self.next = self.next.max(id),
            "assign" | "rename" if id > ROOT => {
                if let Some(old) = self.by_id.get(&id)
                    && self.by_path.get(&old.path) == Some(&id)
                {
                    self.by_path.remove(&old.path.clone());
                }
                if let Some(prev) = self.by_path.insert(path.clone(), id)
                    && prev != id
                    && let Some(r) = self.by_id.get_mut(&prev)
                {
                    r.gone = true;
                }
                self.by_id.insert(id, Rec { path, gone: false });
                self.next = self.next.max(id + 1);
            }
            "drop" => {
                if let Some(r) = self.by_id.get_mut(&id) {
                    r.gone = true;
                    if self.by_path.get(&r.path) == Some(&id) {
                        let p = r.path.clone();
                        self.by_path.remove(&p);
                    }
                }
            }
            _ => {}
        }
    }

    fn log(&mut self, line: &str) {
        if let Some(f) = &mut self.log
            && let Err(e) = f.write_all(line.as_bytes())
        {
            tracing::error!(error = %e, "nodes.log write");
        }
    }

    /// The path of a node, and whether it was unlinked since.
    #[must_use]
    pub fn get(&self, id: u64) -> Option<(&str, bool)> {
        self.by_id.get(&id).map(|r| (r.path.as_str(), r.gone))
    }

    /// The live node of a path.
    #[must_use]
    pub fn id_of(&self, path: &str) -> Option<u64> {
        self.by_path.get(path).copied()
    }

    /// The node of `path`, assigned now if it has none.
    pub fn assign(&mut self, path: &str) -> u64 {
        if let Some(&i) = self.by_path.get(path) {
            return i;
        }
        let i = self.next;
        self.next += 1;
        self.log(&format!("assign {i} {}\n", esc(path)));
        self.by_id.insert(
            i,
            Rec {
                path: path.to_owned(),
                gone: false,
            },
        );
        self.by_path.insert(path.to_owned(), i);
        i
    }

    fn subtree(&self, path: &str) -> Vec<(String, u64)> {
        let pre = format!("{path}/");
        self.by_path
            .iter()
            .filter(|(k, _)| k.starts_with(&pre))
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    }

    /// Marks `path` (and anything under it) unlinked; returns the ids dropped.
    pub fn drop_path(&mut self, path: &str) -> Vec<u64> {
        let mut out = Vec::new();
        let mut all = self.subtree(path);
        if let Some(&i) = self.by_path.get(path) {
            all.push((path.to_owned(), i));
        }
        for (p, i) in all {
            self.by_path.remove(&p);
            if let Some(r) = self.by_id.get_mut(&i) {
                r.gone = true;
            }
            self.log(&format!("drop {i}\n"));
            out.push(i);
        }
        out
    }

    /// Moves `from` (and its subtree) to `to`; whatever was at `to` is dropped (rename replaces it). Returns the
    /// ids dropped at `to`.
    pub fn rename(&mut self, from: &str, to: &str) -> Vec<u64> {
        if from == to {
            return Vec::new();
        }
        let dropped = self.drop_path(to);
        let kids = self.subtree(from);
        if let Some(i) = self.by_path.remove(from) {
            self.set_path(i, to.to_owned());
        }
        for (k, i) in kids {
            self.by_path.remove(&k);
            self.set_path(i, format!("{to}{}", &k[from.len()..]));
        }
        dropped
    }

    fn set_path(&mut self, i: u64, p: String) {
        self.log(&format!("rename {i} {}\n", esc(&p)));
        self.by_path.insert(p.clone(), i);
        if let Some(r) = self.by_id.get_mut(&i) {
            r.path = p;
            r.gone = false;
        }
    }

    /// Drops every live path `keep` refuses (at start: magpie paths absent from the tree, local paths absent from
    /// the backing directory).
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        let gone: Vec<String> = self
            .by_path
            .keys()
            .filter(|p| !p.is_empty() && !keep(p))
            .cloned()
            .collect();
        for p in gone {
            if self.by_path.contains_key(&p) {
                self.drop_path(&p);
            }
        }
    }
}

/// Where the node log lives.
#[must_use]
pub fn log_path(state: &Path) -> PathBuf {
    state.join("nodes.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restart_reproduces_the_mapping() {
        let dir = std::env::temp_dir().join(format!("nodes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = log_path(&dir);
        let (a, b, c, d);
        {
            let mut n = Nodes::open(&p).unwrap();
            a = n.assign("projects");
            b = n.assign("projects/x");
            c = n.assign("projects/x/y.jsonl");
            d = n.assign("odd\nname%0A");
            n.rename("projects/x", "projects/z");
            n.drop_path("todos-none");
            let e = n.assign("projects/gone");
            n.drop_path("projects/gone");
            assert_eq!(n.get(e), Some(("projects/gone", true)));
        }
        let mut n = Nodes::open(&p).unwrap();
        assert_eq!(n.id_of("projects"), Some(a));
        assert_eq!(n.id_of("projects/z"), Some(b));
        assert_eq!(n.id_of("projects/z/y.jsonl"), Some(c));
        assert_eq!(n.id_of("odd\nname%0A"), Some(d));
        assert_eq!(n.id_of("projects/x"), None);
        assert_eq!(n.id_of("projects/gone"), None);
        // never reused
        let f = n.assign("new");
        assert!(f > d + 1);
        // rename onto an existing path drops it
        let g = n.assign("projects/w");
        assert_eq!(n.rename("projects/z", "projects/w"), vec![g]);
        assert_eq!(n.id_of("projects/w"), Some(b));
        assert_eq!(n.get(g), Some(("projects/w", true)));
        drop(n);
        let n = Nodes::open(&p).unwrap();
        assert_eq!(n.id_of("projects/w"), Some(b));
        assert_eq!(n.id_of("projects/w/y.jsonl"), Some(c));
        assert_eq!(n.get(g), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
