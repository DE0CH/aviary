//! The verdict of the frame check's self-test: the check was run with the seeded incompatible frame
//! (app/scripts/pigeon-app/test/incompatible-frame.ndjson) added, and it must have rejected that frame and only that
//! frame. Exit 1 alone is not enough: a crash of node or npm exits non-zero too.

const SEEDED_FILE: &str = "incompatible-frame.ndjson";
const SEEDED_CASE: &str = "seeded incompatible frame";

fn is_protocol_line(l: &str) -> bool {
    l.starts_with("FAIL protocol ") || l.starts_with("ok   protocol ")
}

/// `FAIL protocol …: N server frames, M client frames, 1 invalid`
fn rejected_exactly_one(l: &str) -> bool {
    l.starts_with("FAIL protocol ")
        && l.contains(" server frames, ")
        && l.contains(" client frames, ")
        && l.ends_with(", 1 invalid")
}

/// a failing frame: five spaces, `<file>:<line> in|out …`
fn failure_line(l: &str) -> bool {
    let Some(rest) = l.strip_prefix("     ") else {
        return false;
    };
    let Some((loc, tail)) = rest.split_once(' ') else {
        return false;
    };
    let Some((_, line)) = loc.rsplit_once(':') else {
        return false;
    };
    !loc.starts_with(' ')
        && !line.is_empty()
        && line.bytes().all(|b| b.is_ascii_digit())
        && (tail.starts_with("in ") || tail.starts_with("out "))
}

pub fn verdict(status: i32, log: &str) -> Result<String, String> {
    if status != 1 {
        return Err(format!(
            "the check answered {status} on a seeded incompatible frame (1 expected)"
        ));
    }
    let protocols: Vec<&str> = log.lines().filter(|l| is_protocol_line(l)).collect();
    if protocols.is_empty() {
        return Err("validate-frames.mjs printed no verdict (it did not run to the end)".into());
    }
    if !protocols.iter().all(|l| rejected_exactly_one(l)) {
        return Err(format!(
            "not every protocol build rejected exactly one frame: {}",
            protocols.join(" ")
        ));
    }
    let failures: Vec<&str> = log.lines().filter(|l| failure_line(l)).collect();
    if failures.is_empty() {
        return Err("no failing frame was named".into());
    }
    let others: Vec<&str> = failures
        .iter()
        .copied()
        .filter(|l| !l.contains(&format!("{SEEDED_FILE}:")))
        .collect();
    if !others.is_empty() {
        return Err(format!(
            "frames other than the seeded one were rejected: {}",
            others.join(" ")
        ));
    }
    if !failures.iter().any(|l| l.contains(&format!("({SEEDED_CASE}"))) {
        return Err("the rejected frame is not the seeded case".into());
    }
    Ok(format!(
        "the frame check rejected the seeded frame ({SEEDED_FILE}, '{SEEDED_CASE}') and nothing else"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEEDED: &str = "     /w/app/scripts/pigeon-app/test/incompatible-frame.ndjson:1 out hello.rejected (seeded incompatible frame: hello.rejected with a reason no client knows): reason: invalid enum value\n";

    fn rejected() -> String {
        format!(
            "check-frames: corpus\nFAIL protocol 0123456789 (zod 4.1.0 + zod-aot 0.4.0) of this checkout: 412 server frames, 37 client frames, 1 invalid\n{SEEDED}"
        )
    }

    #[test]
    fn only_the_seeded_frame_rejected_passes() {
        assert!(verdict(1, &rejected()).is_ok());
    }

    #[test]
    fn a_crash_with_no_verdict_fails() {
        assert!(
            verdict(1, "npm ERR! code E404\n")
                .unwrap_err()
                .contains("printed no verdict")
        );
    }

    #[test]
    fn another_status_fails() {
        assert!(verdict(0, &rejected()).unwrap_err().contains("answered 0"));
        assert!(verdict(2, &rejected()).unwrap_err().contains("answered 2"));
    }

    #[test]
    fn another_frame_rejected_as_well_or_instead_fails() {
        let other = "     /w/frames/corpus/pigeon-frames.ndjson:9 out session.update: bad\n";
        let both = rejected().replace("1 invalid", "2 invalid") + other;
        assert!(verdict(1, &both).unwrap_err().contains("exactly one frame"));
        let instead = rejected().replace(SEEDED, other);
        assert!(verdict(1, &instead).unwrap_err().contains("other than the seeded one"));
    }
}
