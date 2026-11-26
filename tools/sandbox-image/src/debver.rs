//! Debian's version order (deb-version(7), as dpkg compares): `[epoch:]upstream[-revision]`, each part compared in
//! runs of non-digits (letters before everything else, `~` before even the end) and runs of digits (as numbers).

use std::cmp::Ordering;

fn order(c: Option<u8>) -> i32 {
    match c {
        Some(b'~') => -1,
        None => 0,
        Some(c) if c.is_ascii_alphabetic() => i32::from(c),
        Some(c) => i32::from(c) + 256,
    }
}

/// one part (upstream or revision) against another
fn part(left: &str, right: &str) -> Ordering {
    let (l, r) = (left.as_bytes(), right.as_bytes());
    let (mut li, mut ri) = (0, 0);
    while li < l.len() || ri < r.len() {
        // the non-digit run
        while (li < l.len() && !l[li].is_ascii_digit()) || (ri < r.len() && !r[ri].is_ascii_digit()) {
            let lc = l.get(li).copied().filter(|c| !c.is_ascii_digit());
            let rc = r.get(ri).copied().filter(|c| !c.is_ascii_digit());
            let by_char = order(lc).cmp(&order(rc));
            if by_char != Ordering::Equal {
                return by_char;
            }
            if lc.is_some() {
                li += 1;
            }
            if rc.is_some() {
                ri += 1;
            }
        }
        // the digit run, as a number (leading zeros do not count)
        let (l0, r0) = (li, ri);
        while li < l.len() && l[li].is_ascii_digit() {
            li += 1;
        }
        while ri < r.len() && r[ri].is_ascii_digit() {
            ri += 1;
        }
        let ln = std::str::from_utf8(&l[l0..li]).unwrap_or("").trim_start_matches('0');
        let rn = std::str::from_utf8(&r[r0..ri]).unwrap_or("").trim_start_matches('0');
        let by_number = ln.len().cmp(&rn.len()).then_with(|| ln.cmp(rn));
        if by_number != Ordering::Equal {
            return by_number;
        }
    }
    Ordering::Equal
}

/// epoch, upstream, revision
fn split(v: &str) -> (u64, &str, &str) {
    let (epoch, rest) = match v.split_once(':') {
        Some((e, r)) => (e.parse().unwrap_or(0), r),
        None => (0, v),
    };
    match rest.rsplit_once('-') {
        Some((u, r)) => (epoch, u, r),
        None => (epoch, rest, ""),
    }
}

pub fn compare(a: &str, b: &str) -> Ordering {
    let (ea, ua, ra) = split(a);
    let (eb, ub, rb) = split(b);
    ea.cmp(&eb).then_with(|| part(ua, ub)).then_with(|| part(ra, rb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::{Equal, Greater, Less};

    #[test]
    fn dpkgs_own_examples() {
        for (a, b, want) in [
            ("1.0", "1.0", Equal),
            ("1.0", "1.1", Less),
            ("1.10", "1.9", Greater),
            ("1.0~rc1", "1.0", Less),
            ("1.0~~", "1.0~", Less),
            ("1.0a", "1.0", Greater),
            ("1.0a", "1.0.", Less),
            ("1:0.1", "2.0", Greater),
            ("1.0-1", "1.0-2", Less),
            ("1.0-10", "1.0-9", Greater),
            ("1.01", "1.1", Equal),
        ] {
            assert_eq!(compare(a, b), want, "{a} vs {b}");
        }
    }

    #[test]
    fn dockers_versions() {
        assert_eq!(
            compare("5:29.10.1-1~debian.13~trixie", "5:29.9.0-1~debian.13~trixie"),
            Greater
        );
        assert_eq!(
            compare("5:29.10.1-1~debian.13~trixie", "5:29.10.1-1~debian.13~trixie"),
            Equal
        );
        assert_eq!(compare("2.4.2-4~debian.13~trixie", "2.4.10-1~debian.13~trixie"), Less);
    }
}
