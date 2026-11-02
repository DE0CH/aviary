//! The uuids of transcript lines magpie has acknowledged (supervisor 8.2): every `cc/write` magpie answered to a
//! `.jsonl` file is scanned for `"uuid":"…"`, so a turn's end report can wait until magpie holds the lines its
//! stdout showed. A line split across two writes is found through the tail kept from the previous write.

use std::collections::{HashMap, HashSet, VecDeque};

/// The uuids kept (the oldest are forgotten first).
const KEPT: usize = 65_536;
/// The bytes of a write kept to complete a uuid split across two writes.
const CARRY: usize = 64;
/// How much of the main transcript a wait that misses uuids scans (a restart's earlier writes).
pub const TAIL_SCAN: u64 = 1024 * 1024;

const KEY: &[u8] = b"\"uuid\":\"";

/// Every `"uuid":"<36 characters>"` in `data`.
#[must_use]
pub fn uuids_in(data: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = data.get(i..).and_then(|d| d.windows(KEY.len()).position(|w| w == KEY)) {
        let start = i + p + KEY.len();
        if let Some(v) = data.get(start..start + 36)
            && data.get(start + 36) == Some(&b'"')
            && v.iter().all(|b| b.is_ascii_hexdigit() || *b == b'-')
        {
            out.push(String::from_utf8_lossy(v).into_owned());
        }
        i = start;
    }
    out
}

/// What has been seen.
#[derive(Debug, Default)]
pub struct Seen {
    set: HashSet<String>,
    order: VecDeque<String>,
    /// Per path: where the last write ended and its last bytes.
    carry: HashMap<String, (u64, Vec<u8>)>,
}

impl Seen {
    /// Records the uuids of a write of `data` at `offset` to `path` that magpie answered.
    pub fn wrote(&mut self, path: &str, offset: u64, data: &[u8]) {
        let mut buf = Vec::new();
        if let Some((end, tail)) = self.carry.get(path)
            && *end == offset
        {
            buf.extend_from_slice(tail);
        }
        buf.extend_from_slice(data);
        for u in uuids_in(&buf) {
            self.add(u);
        }
        let keep = buf.len().saturating_sub(CARRY);
        self.carry
            .insert(path.to_owned(), (offset + data.len() as u64, buf.split_off(keep)));
    }

    /// Records uuids found another way (a scan of what magpie holds).
    pub fn add(&mut self, u: String) {
        if self.set.insert(u.clone()) {
            self.order.push_back(u);
            while self.order.len() > KEPT {
                if let Some(old) = self.order.pop_front() {
                    self.set.remove(&old);
                }
            }
        }
    }

    /// Whether the lines of `uuids` are stored: every one seen, or the last one (a transcript is appended in
    /// order, so the last line stored means the earlier ones are).
    #[must_use]
    pub fn holds(&self, uuids: &[String]) -> bool {
        uuids.last().is_none_or(|l| self.set.contains(l)) || uuids.iter().all(|u| self.set.contains(u))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0190f6c2-1111-7000-8000-000000000001";
    const B: &str = "0190f6c2-2222-7000-8000-000000000002";

    #[test]
    fn uuids_are_found_and_parent_uuids_are_not_taken_for_them() {
        let line = format!(
            "{{\"parentUuid\":\"{A}\",\"type\":\"user\",\"message\":{{\"content\":\"say \\\"uuid\\\":\\\"x\\\"\"}},\"uuid\":\"{B}\"}}\n"
        );
        assert_eq!(uuids_in(line.as_bytes()), vec![B.to_owned()]);
        assert_eq!(uuids_in(b"\"uuid\":\"short\""), Vec::<String>::new());
        assert_eq!(uuids_in(b""), Vec::<String>::new());
    }

    #[test]
    fn a_uuid_split_across_two_writes_is_found() {
        let line = format!("{{\"type\":\"assistant\",\"uuid\":\"{A}\"}}\n");
        let (a, b) = line.as_bytes().split_at(30);
        let mut s = Seen::default();
        s.wrote("p.jsonl", 0, a);
        assert!(!s.holds(&[A.to_owned()]));
        s.wrote("p.jsonl", a.len() as u64, b);
        assert!(s.holds(&[A.to_owned()]));
        // a write elsewhere does not complete it
        let mut s = Seen::default();
        s.wrote("p.jsonl", 0, a);
        s.wrote("p.jsonl", 999, b);
        assert!(!s.holds(&[A.to_owned()]));
    }

    #[test]
    fn the_last_uuid_stands_for_the_earlier_ones() {
        let mut s = Seen::default();
        s.add(B.to_owned());
        assert!(s.holds(&[A.to_owned(), B.to_owned()]));
        assert!(!s.holds(&[B.to_owned(), A.to_owned()]));
        assert!(s.holds(&[]));
    }
}
