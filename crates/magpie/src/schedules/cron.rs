//! The rule of a schedule (pure): five-field cron expressions, times in a zone, the next fire, and the reading of a
//! rule in plain Hong Kong Chinese (`describe`). The old stack's router has the same rules in JavaScript
//! (DE0CH/claudecodeui `jarvis/schedules.mjs`); both are checked against the same vectors (`tests/schedules.rs`
//! here, `jarvis/test/schedules.test.mjs` there), so the AI and staff read the same words on either stack.
//!
//! Cron: minute (0-59), hour (0-23), day of month (1-31), month (1-12 or JAN-DEC), day of week (0-7 or SUN-SAT, 0
//! and 7 Sunday); each field `*`, `a`, `a-b`, a list of those, each with an optional `/step`. As in Vixie cron, when
//! both day fields are restricted a day matching either fires. Times are wall-clock times in the schedule's zone:
//! a time that does not exist (a daylight-saving gap) is skipped, one that exists twice fires at its first.

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, Offset, TimeZone, Timelike};
use chrono_tz::Tz;

/// The zone a schedule has when none is given.
pub const DEFAULT_TZ: &str = "Asia/Hong_Kong";

/// How far ahead a rule's next fire is looked for (a rule with none is refused).
const SEARCH_DAYS: u32 = 366 * 5;

/// A parsed cron rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
    days_any: bool,
    weekdays_any: bool,
}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const WEEKDAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
const WEEKDAY_ZH: [&str; 7] = ["日", "一", "二", "三", "四", "五", "六"];

fn value(s: &str, lo: u32, hi: u32, names: &[&str], first: u32) -> Result<u32, String> {
    let up = s.to_ascii_uppercase();
    if let Some(i) = names.iter().position(|n| *n == up) {
        return Ok(first + u32::try_from(i).unwrap_or(0));
    }
    let v: u32 = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    if v < lo || v > hi {
        return Err(format!("{v} is outside {lo}-{hi}"));
    }
    Ok(v)
}

/// One field: the bit mask of its values (bit n = value n) and whether it starts with `*`.
fn field(s: &str, lo: u32, hi: u32, names: &[&str], first: u32) -> Result<(u64, bool), String> {
    if s.is_empty() {
        return Err("an empty field".into());
    }
    let mut mask = 0u64;
    for part in s.split(',') {
        let (base, step) = match part.split_once('/') {
            Some((b, st)) => {
                let st: u32 = st.parse().map_err(|_| format!("`{st}` is not a step"))?;
                if st == 0 {
                    return Err("a step of 0".into());
                }
                (b, st)
            }
            None => (part, 1),
        };
        let (a, b) = if base == "*" {
            (lo, hi)
        } else if let Some((a, b)) = base.split_once('-') {
            (value(a, lo, hi, names, first)?, value(b, lo, hi, names, first)?)
        } else {
            let a = value(base, lo, hi, names, first)?;
            // `a/step` runs from a to the end of the range, as in Vixie cron
            (a, if part.contains('/') { hi } else { a })
        };
        if a > b {
            return Err(format!("the range {a}-{b} runs backwards"));
        }
        let mut v = a;
        while v <= b {
            mask |= 1 << v;
            // a step past the range ends the field (never wraps around to values below `a`)
            let Some(next) = v.checked_add(step) else { break };
            v = next;
        }
    }
    Ok((mask, s.starts_with('*')))
}

/// Parses a five-field rule.
///
/// # Errors
/// A sentence saying what is wrong.
pub fn parse(expr: &str) -> Result<Rule, String> {
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 {
        return Err("a cron rule has five fields: minute hour day-of-month month day-of-week".into());
    }
    let (minutes, _) = field(f[0], 0, 59, &[], 0).map_err(|e| format!("minute: {e}"))?;
    let (hours, _) = field(f[1], 0, 23, &[], 0).map_err(|e| format!("hour: {e}"))?;
    let (days, days_any) = field(f[2], 1, 31, &[], 0).map_err(|e| format!("day of month: {e}"))?;
    let (months, _) = field(f[3], 1, 12, &MONTHS, 1).map_err(|e| format!("month: {e}"))?;
    let (mut weekdays, weekdays_any) = field(f[4], 0, 7, &WEEKDAYS, 0).map_err(|e| format!("day of week: {e}"))?;
    if weekdays & (1 << 7) != 0 {
        weekdays = (weekdays & !(1 << 7)) | 1;
    }
    Ok(Rule {
        minutes,
        hours: u32::try_from(hours).unwrap_or(0),
        days: u32::try_from(days).unwrap_or(0),
        months: u16::try_from(months).unwrap_or(0),
        weekdays: u8::try_from(weekdays).unwrap_or(0),
        days_any,
        weekdays_any,
    })
}

/// The zone of an IANA name.
///
/// # Errors
/// When the name is not a zone.
pub fn zone(name: &str) -> Result<Tz, String> {
    name.parse::<Tz>()
        .map_err(|_| format!("`{name}` is not an IANA time zone (e.g. Asia/Hong_Kong)"))
}

fn bits(mask: u64, lo: u32, hi: u32) -> Vec<u32> {
    (lo..=hi).filter(|v| mask & (1 << v) != 0).collect()
}

impl Rule {
    fn day_matches(&self, d: NaiveDate) -> bool {
        if self.months & (1 << d.month()) == 0 {
            return false;
        }
        let dom = self.days & (1 << d.day()) != 0;
        let dow = self.weekdays & (1 << d.weekday().num_days_from_sunday()) != 0;
        if self.days_any || self.weekdays_any {
            dom && dow
        } else {
            dom || dow
        }
    }

    /// The first fire strictly after `after` (Unix seconds), in `tz`.
    #[must_use]
    pub fn next_after(&self, tz: Tz, after: i64) -> Option<i64> {
        let start = DateTime::from_timestamp(after, 0)?.with_timezone(&tz).naive_local();
        let mut date = start.date();
        let hours = bits(u64::from(self.hours), 0, 23);
        let minutes = bits(self.minutes, 0, 59);
        for _ in 0..SEARCH_DAYS {
            if self.day_matches(date) {
                for hour in &hours {
                    for minute in &minutes {
                        let Some(wall) = date.and_hms_opt(*hour, *minute, 0) else {
                            continue;
                        };
                        if let Some(t) = tz.from_local_datetime(&wall).earliest()
                            && t.timestamp() > after
                        {
                            return Some(t.timestamp());
                        }
                    }
                }
            }
            date = date.succ_opt()?;
        }
        None
    }

    /// The shortest gap between consecutive fires over the next `n` fires after `after` (seconds); `None` when it
    /// fires fewer than twice.
    #[must_use]
    pub fn min_gap(&self, tz: Tz, after: i64, n: usize) -> Option<i64> {
        let mut prev = self.next_after(tz, after)?;
        let mut gap: Option<i64> = None;
        for _ in 1..n {
            let Some(t) = self.next_after(tz, prev) else { break };
            gap = Some(gap.map_or(t - prev, |g| g.min(t - prev)));
            prev = t;
        }
        gap
    }

    fn time_part(&self) -> Option<String> {
        let mins = bits(self.minutes, 0, 59);
        let hrs = bits(u64::from(self.hours), 0, 23);
        let every_hour = hrs.len() == 24;
        if let [minute] = mins[..] {
            if every_hour {
                return Some(format!("每小時第 {minute} 分"));
            }
            if hrs.len() <= 6 {
                return Some(
                    hrs.iter()
                        .map(|hour| format!("{hour:02}:{minute:02}"))
                        .collect::<Vec<_>>()
                        .join("、"),
                );
            }
            return even_step(&hrs, 0, 23).map(|step| format!("每 {step} 小時（第 {minute} 分）"));
        }
        if every_hour
            && mins.first() == Some(&0)
            && let Some(step) = even_step(&mins, 0, 59)
        {
            return Some(format!("每 {step} 分鐘"));
        }
        None
    }

    fn day_part(&self) -> String {
        let all_months = self.months == 0b1_1111_1111_1110;
        let weekday_list = bits(u64::from(self.weekdays), 0, 6);
        let day_list = bits(u64::from(self.days), 1, 31);
        let weekdays = if self.weekdays_any {
            None
        } else if weekday_list == [1, 2, 3, 4, 5] {
            Some("每個工作日".to_owned())
        } else if weekday_list == [0, 6] {
            Some("每個週末".to_owned())
        } else {
            let names: Vec<&str> = weekday_list.iter().map(|d| WEEKDAY_ZH[*d as usize]).collect();
            Some(format!("每個星期{}", names.join("、")))
        };
        let days = (!self.days_any).then(|| day_list.iter().map(ToString::to_string).collect::<Vec<_>>().join("、"));
        let months = (!all_months).then(|| {
            bits(u64::from(self.months), 1, 12)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("、")
        });
        match (months, days, weekdays) {
            (None, None, None) => "每天".to_owned(),
            (None, None, Some(w)) => w,
            (None, Some(d), None) => format!("每月 {d} 日"),
            (None, Some(d), Some(w)) => format!("每月 {d} 日及{w}"),
            (Some(m), None, None) => format!("每年 {m} 月每天"),
            (Some(m), None, Some(w)) => format!("每年 {m} 月{w}"),
            (Some(m), Some(d), None) => format!("每年 {m} 月 {d} 日"),
            (Some(m), Some(d), Some(w)) => format!("每年 {m} 月 {d} 日及{w}"),
        }
    }

    /// The rule in plain Hong Kong Chinese (`每個工作日 08:30`); a rule this reading cannot put in words is quoted.
    #[must_use]
    pub fn describe(&self, expr: &str) -> String {
        let day = self.day_part();
        match self.time_part() {
            Some(t) if day == "每天" && t.starts_with('每') => t,
            Some(t) => format!("{day} {t}"),
            None => format!("按規則「{}」", expr.split_whitespace().collect::<Vec<_>>().join(" ")),
        }
    }
}

/// The step of values that run evenly from `lo` (`0, 15, 30, 45`), when they do and there are at least two.
fn even_step(v: &[u32], lo: u32, hi: u32) -> Option<u32> {
    if v.len() < 2 || v[0] != lo {
        return None;
    }
    let step = v[1] - v[0];
    let even = v.windows(2).all(|w| w[1] - w[0] == step);
    // the values must be every step up to the end of the range
    (even && v[v.len() - 1] + step > hi).then_some(step)
}

/// A zone's name in words, for the zones staff use; `None` for Hong Kong (the default says nothing).
#[must_use]
pub fn zone_words(tz: &str) -> Option<String> {
    let w = match tz {
        "Asia/Hong_Kong" => return None,
        "Asia/Shanghai" => "北京時間",
        "Asia/Taipei" => "台北時間",
        "Asia/Macau" => "澳門時間",
        "Asia/Singapore" => "新加坡時間",
        "Asia/Tokyo" => "東京時間",
        "Asia/Seoul" => "首爾時間",
        "Europe/London" => "倫敦時間",
        "America/New_York" => "紐約時間",
        "UTC" | "Etc/UTC" => "UTC",
        other => return Some(other.to_owned()),
    };
    Some(w.to_owned())
}

/// A cron rule's reading with its zone (`每個工作日 08:30`, `每天 09:00（倫敦時間）`).
#[must_use]
pub fn describe_cron(rule: &Rule, expr: &str, tz: &str) -> String {
    let d = rule.describe(expr);
    match zone_words(tz) {
        Some(z) => format!("{d}（{z}）"),
        None => d,
    }
}

/// A one-off's reading: `11月7日（星期五）09:00`, with the year when it is not `now`'s, and the zone.
#[must_use]
pub fn describe_once(at: i64, tz: Tz, now: i64) -> String {
    let Some(t) = DateTime::from_timestamp(at, 0) else {
        return String::new();
    };
    let l = t.with_timezone(&tz).naive_local();
    let this_year = DateTime::from_timestamp(now, 0).map_or(l.year(), |n| n.with_timezone(&tz).year());
    let year = if l.year() == this_year {
        String::new()
    } else {
        format!("{}年", l.year())
    };
    let wd = WEEKDAY_ZH[l.weekday().num_days_from_sunday() as usize];
    let d = format!(
        "{year}{}月{}日（星期{wd}）{:02}:{:02}",
        l.month(),
        l.day(),
        l.hour(),
        l.minute()
    );
    match zone_words(tz.name()) {
        Some(z) => format!("{d}（{z}）"),
        None => d,
    }
}

/// A Unix time as a local time with its offset in `tz`: `2026-11-07T08:30:00+08:00`.
#[must_use]
pub fn local(t: i64, tz: Tz) -> String {
    let Some(u) = DateTime::from_timestamp(t, 0) else {
        return String::new();
    };
    let l = u.with_timezone(&tz);
    let off = l.offset().fix().local_minus_utc();
    let n = l.naive_local();
    let sign = if off < 0 { '-' } else { '+' };
    let off = off.abs();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
        n.year(),
        n.month(),
        n.day(),
        n.hour(),
        n.minute(),
        n.second(),
        off / 3600,
        (off % 3600) / 60
    )
}

fn num(s: &str, what: &str) -> Result<u32, String> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{what} is not a number"));
    }
    s.parse().map_err(|_| format!("{what} is not a number"))
}

/// A time as the AI writes it: `2026-11-07T09:00+08:00`, `2026-11-07T09:00:00Z`, `2026-11-07 09:00` (local in
/// `tz`), seconds and fractions optional. Unix seconds (a fraction is dropped).
///
/// # Errors
/// A sentence saying what is wrong.
pub fn parse_time(s: &str, tz: Tz) -> Result<i64, String> {
    let bad = || format!("`{s}` is not a time like 2026-11-07T09:00+08:00");
    let s = s.trim();
    // ASCII only: what follows cuts at byte offsets, and a time in words (`2026年11月7日 09:00`) must be refused,
    // not cut inside a character (a panic ends the process)
    if s.len() < 16 || !s.is_ascii() {
        return Err(bad());
    }
    let (date, rest) = s.split_at(10);
    let mut parts = date.split('-');
    let (Some(year), Some(month), Some(day), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(bad());
    };
    let date = NaiveDate::from_ymd_opt(
        i32::try_from(num(year, "the year")?).map_err(|_| bad())?,
        num(month, "the month")?,
        num(day, "the day")?,
    )
    .ok_or_else(bad)?;
    let rest = rest
        .strip_prefix('T')
        .or_else(|| rest.strip_prefix(' '))
        .ok_or_else(bad)?;
    // the clock: up to the zone designator
    let zone_at = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (clock, zone_s) = rest.split_at(zone_at);
    let clock = clock.split('.').next().unwrap_or("");
    let mut fields = clock.split(':');
    let hour = num(fields.next().unwrap_or(""), "the hour")?;
    let minute = num(fields.next().unwrap_or(""), "the minute")?;
    let second = match fields.next() {
        Some(x) => num(x, "the second")?,
        None => 0,
    };
    if fields.next().is_some() {
        return Err(bad());
    }
    let naive: NaiveDateTime = date.and_hms_opt(hour, minute, second.min(59)).ok_or_else(bad)?;
    if zone_s.is_empty() {
        return tz
            .from_local_datetime(&naive)
            .earliest()
            .map(|t| t.timestamp())
            .ok_or_else(|| format!("{s} does not exist in {} (a clock change)", tz.name()));
    }
    let offset = if zone_s.eq_ignore_ascii_case("z") {
        0
    } else {
        let sign: i64 = if zone_s.starts_with('-') { -1 } else { 1 };
        let digits: String = zone_s[1..].chars().filter(|c| *c != ':').collect();
        if digits.len() != 4 && digits.len() != 2 {
            return Err(bad());
        }
        let oh = i64::from(num(&digits[..2], "the offset")?);
        let om = if digits.len() == 4 {
            i64::from(num(&digits[2..], "the offset")?)
        } else {
            0
        };
        if oh > 18 || om > 59 {
            return Err(bad());
        }
        sign * (oh * 3600 + om * 60)
    };
    Ok(naive.and_utc().timestamp() - offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HK: Tz = chrono_tz::Asia::Hong_Kong;

    fn t(s: &str) -> i64 {
        parse_time(s, HK).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn weekdays_at_half_past_eight() {
        let r = parse("30 8 * * 1-5").unwrap();
        assert_eq!(r.describe("30 8 * * 1-5"), "每個工作日 08:30");
        // Friday 2026-11-06 09:00 → Monday 08:30
        assert_eq!(
            local(r.next_after(HK, t("2026-11-06T09:00+08:00")).unwrap(), HK),
            "2026-11-09T08:30:00+08:00"
        );
        assert_eq!(
            local(r.next_after(HK, t("2026-11-06T08:00+08:00")).unwrap(), HK),
            "2026-11-06T08:30:00+08:00"
        );
    }

    #[test]
    fn readings() {
        for (expr, want) in [
            ("0 9 * * *", "每天 09:00"),
            ("0 9 * * 0,6", "每個週末 09:00"),
            ("0 18 * * MON", "每個星期一 18:00"),
            ("0 9 * * 1,3,5", "每個星期一、三、五 09:00"),
            ("0 9 1 * *", "每月 1 日 09:00"),
            ("0 9 1,15 * *", "每月 1、15 日 09:00"),
            ("0 9 1 1 *", "每年 1 月 1 日 09:00"),
            ("0 * * * *", "每小時第 0 分"),
            ("*/30 * * * *", "每 30 分鐘"),
            ("0 9,13,18 * * 1-5", "每個工作日 09:00、13:00、18:00"),
            ("0 */2 * * *", "每 2 小時（第 0 分）"),
            ("7,19 3 * * *", "按規則「7,19 3 * * *」"),
        ] {
            assert_eq!(parse(expr).unwrap().describe(expr), want, "{expr}");
        }
        assert_eq!(
            describe_cron(&parse("0 9 * * *").unwrap(), "0 9 * * *", "Europe/London"),
            "每天 09:00（倫敦時間）"
        );
    }

    #[test]
    fn day_fields_or_when_both_are_restricted() {
        let r = parse("0 9 13 * 5").unwrap();
        // Friday 2026-11-06 → Friday 13 November? Friday the 6th at 09:00 matches the weekday
        assert_eq!(
            local(r.next_after(HK, t("2026-11-06T08:00+08:00")).unwrap(), HK),
            "2026-11-06T09:00:00+08:00"
        );
        // after it: Friday 13th (both) comes before the next Friday? 13 Nov 2026 is a Friday
        assert_eq!(
            local(r.next_after(HK, t("2026-11-06T10:00+08:00")).unwrap(), HK),
            "2026-11-13T09:00:00+08:00"
        );
        let r = parse("0 9 13 * *").unwrap();
        assert_eq!(
            local(r.next_after(HK, t("2026-11-14T00:00+08:00")).unwrap(), HK),
            "2026-12-13T09:00:00+08:00"
        );
    }

    #[test]
    fn refused_rules() {
        for bad in [
            "",
            "* * * *",
            "60 * * * *",
            "0 24 * * *",
            "0 0 0 * *",
            "0 0 * 13 *",
            "0 0 * * 8",
            "*/0 * * * *",
            "5-1 * * * *",
            "a * * * *",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        // February 30th never comes
        assert_eq!(parse("0 0 30 2 *").unwrap().next_after(HK, 0), None);
    }

    #[test]
    fn gaps() {
        assert_eq!(parse("*/5 * * * *").unwrap().min_gap(HK, 0, 50), Some(300));
        assert_eq!(parse("0,10 9 * * *").unwrap().min_gap(HK, 0, 50), Some(600));
        assert_eq!(parse("30 8 * * 1-5").unwrap().min_gap(HK, 0, 50), Some(86_400));
    }

    #[test]
    fn a_step_past_the_range_names_only_its_start() {
        // `5/4294967295`: the step must not wrap around into values below the start
        let r = parse("5/4294967295 8 * * *").unwrap();
        assert_eq!(bits(r.minutes, 0, 59), [5]);
        let r = parse("*/4294967295 8 * * *").unwrap();
        assert_eq!(bits(r.minutes, 0, 59), [0]);
    }

    #[test]
    fn times() {
        assert_eq!(t("2026-11-07T09:00+08:00"), t("2026-11-07T01:00:00Z"));
        assert_eq!(t("2026-11-07 09:00"), t("2026-11-07T09:00:00+08:00"));
        assert_eq!(t("2026-11-07T09:00:30.5+0800"), t("2026-11-07T01:00:30Z"));
        for bad in [
            "tomorrow",
            "2026-11-07",
            "2026-13-07T09:00",
            "2026-11-07T25:00",
            "2026-11-07T09:00+8",
        ] {
            assert!(parse_time(bad, HK).is_err(), "{bad}");
        }
        // a time in words (the AI writes 年月日, full-width digits or an odd offset): refused, never a panic,
        // which would end the process (the release profile aborts on one)
        for bad in [
            "2026年11月7日 09:00",
            "2026年11月7日上午九時正",
            "２０２６-11-07T09:00",
            "2026-11-07T09:00+a\u{e9}1",
            "2026-11-07T09:00+\u{e9}",
        ] {
            assert!(parse_time(bad, HK).is_err(), "{bad}");
        }
        assert_eq!(local(t("2026-11-07T01:00:00Z"), HK), "2026-11-07T09:00:00+08:00");
        let london = zone("Europe/London").unwrap();
        assert_eq!(local(t("2026-07-01T12:00:00Z"), london), "2026-07-01T13:00:00+01:00");
        assert!(zone("Mars/Base").is_err());
    }

    #[test]
    fn one_offs() {
        let now = t("2026-11-06T10:00+08:00");
        assert_eq!(
            describe_once(t("2026-11-09T15:00+08:00"), HK, now),
            "11月9日（星期一）15:00"
        );
        assert_eq!(
            describe_once(t("2027-01-04T09:00+08:00"), HK, now),
            "2027年1月4日（星期一）09:00"
        );
    }

    #[test]
    fn daylight_saving() {
        let london = zone("Europe/London").unwrap();
        // 2027-03-28 01:30 does not exist in London: the next 01:30 is the 29th
        let r = parse("30 1 * * *").unwrap();
        let after = parse_time("2027-03-27T12:00:00Z", london).unwrap();
        assert_eq!(
            local(r.next_after(london, after).unwrap(), london),
            "2027-03-29T01:30:00+01:00"
        );
    }
}
