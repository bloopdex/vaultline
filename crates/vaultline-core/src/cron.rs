//! Schedule evaluation (Phase 5). The configuration carries two cron
//! strings — `application.schedule` (backup runs) and
//! `application.verification.schedule` (verification runs) — both
//! 5-field expressions (minute, hour, day-of-month, month, day-of-week).
//!
//! This module wraps the audited `cron` crate for validation and
//! next-occurrence computation, and translates expressions into systemd
//! `OnCalendar` values for the generated timer units. Six/seven-field
//! expressions (seconds, years) are rejected at this boundary: accepting a
//! shape we cannot schedule would be dishonest.
//!
//! `cron`'s `after()` is exclusive (it searches from the anchor plus one
//! second) — the due-ness rule "the next occurrence strictly after the
//! last run has arrived" is exactly what scheduling needs.
//!
//! Recorded finding (2026-09-07): cron 0.17's longhand parser accepts only
//! 6/7-field expressions (seconds first) — 5-field input is rejected
//! outright (the crate's own tests pin 4-field invalid, 6-field valid).
//! The user-facing contract stays 5-field; this module prepends the fixed
//! second (`0 `) before validation, which is exactly standard 5-field
//! semantics. The crate's `?` (Quartz "unspecified") on day fields maps to
//! `*` for OnCalendar translation.

use chrono::{DateTime, Utc};
use cron::Schedule;
use std::str::FromStr;

/// Parse and validate a 5-field cron expression (minute hour day-of-month
/// month day-of-week). The audited `cron` crate evaluates the expression
/// with seconds fixed to 0.
///
/// One constraint narrows the accepted language, by evidence: when BOTH
/// day fields are restricted (e.g. `13` and `6`), the crate evaluates them
/// with AND semantics (verified empirically: `0 9 13 * 6` next fires on a
/// Friday-the-13th), while standard cron and systemd OnCalendar OR them.
/// The divergence would make `schedule run` and the generated timer fire
/// at different times for the same definition — so both-restricted day
/// fields are rejected with an explanation. One day field at a time, with
/// the other `*` (or `?`), covers the accepted language.
pub fn parse_schedule(expression: &str) -> Result<Schedule, String> {
    let parts: Vec<&str> = expression.split_whitespace().collect();
    if parts.len() != 5 {
        return Err(format!(
            "schedule \"{expression}\" has {} field(s); exactly 5 are accepted (minute hour day-of-month month day-of-week)",
            parts.len()
        ));
    }
    let restricted = |field: &str| field != "*" && field != "?";
    if restricted(parts[2]) && restricted(parts[4]) {
        return Err(format!(
            "schedule \"{expression}\" restricts both day-of-month and day-of-week — evaluation semantics differ between cron implementations (the engine ANDs them; systemd timers OR them), so the same definition could fire at different times. Restrict one day field and leave the other \"*\""
        ));
    }
    Schedule::from_str(&format!("0 {expression}"))
        .map_err(|e| format!("invalid schedule \"{expression}\": {e}"))
}

/// The first occurrence strictly after `after`; `None` when the schedule
/// has none (an empty match set).
pub fn next_after(schedule: &Schedule, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    schedule.after(&after).next()
}

/// Whether a datetime matches the schedule, checked through the
/// occurrence iterator (the same code path `next_after` uses — one
/// evaluation rule everywhere). Both-restricted day fields never reach
/// this point (rejected at parse), so the AND/OR question does not arise.
pub fn includes(schedule: &Schedule, at: DateTime<Utc>) -> bool {
    next_after(schedule, at - chrono::Duration::seconds(1)) == Some(at)
}

/// Translate a 5-field cron expression into a systemd `OnCalendar=`
/// value. day-of-month and day-of-week keep cron's OR semantics (systemd
/// ORs weekday and date the same way). Supported tokens per field: `*`,
/// numbers, three-letter month/weekday names (case-insensitive), comma
/// lists, `a-b` ranges, and `/step` over `*` or a range. Anything else is
/// an error — a schedule that cannot be translated is reported, never
/// approximated.
pub fn to_on_calendar(expression: &str) -> Result<String, String> {
    // The cron crate is the validity authority; translation only reformats
    // what already parsed.
    parse_schedule(expression)?;
    let fields: Vec<&str> = expression.split_whitespace().collect();

    let minute = translate_number_field(fields[0], 0, 59, false)?;
    let hour = translate_number_field(fields[1], 0, 23, false)?;
    let dom = translate_number_field(fields[2], 1, 31, true)?;
    let month = translate_month_field(fields[3])?;
    let dow = translate_dow_field(fields[4])?;

    // The date part carries month in every shape: a month constraint is
    // never dropped. (The both-restricted shape cannot reach this point —
    // parse_schedule rejects it.)
    let date = match (dom.as_str(), dow.as_str()) {
        ("*", "*") => format!("*-{month}-*"),
        ("*", d) => format!("{d} *-{month}-*"),
        (m, "*") => format!("*-{month}-{m}"),
        (m, d) => format!("{d} *-{month}-{m}"),
    };
    Ok(format!("{date} {hour}:{minute}:00"))
}

/// Translate one numeric cron field (minute/hour/day-of-month): numbers,
/// `*`, lists, ranges, and steps over `*` or a range.
fn translate_number_field(field: &str, min: u32, max: u32, pad: bool) -> Result<String, String> {
    let tokens: Vec<&str> = field.split(',').collect();
    let mut out: Vec<String> = Vec::new();
    for token in tokens {
        out.push(translate_numeric_token(token, min, max, pad)?);
    }
    Ok(out.join(","))
}

fn translate_numeric_token(token: &str, min: u32, max: u32, pad: bool) -> Result<String, String> {
    // Quartz `?` (unspecified) is semantically `*`; only day fields accept
    // it, and only on its own (the crate's validation already rejected any
    // other placement).
    if token == "?" {
        return Ok("*".to_string());
    }
    if token.contains('?') {
        return Err(format!(
            "cannot translate \"{token}\" to OnCalendar: \"?\" must stand alone"
        ));
    }
    let (base, step) = match token.split_once('/') {
        Some((base, step)) => (base, Some(step)),
        None => (token, None),
    };
    let fmt = |n: u32| {
        if pad {
            format!("{n:02}")
        } else {
            n.to_string()
        }
    };
    let rendered = if base == "*" {
        "*".to_string()
    } else if let Some((start, end)) = base.split_once('-') {
        let (start, end) = (
            parse_in_range(start, min, max, token)?,
            parse_in_range(end, min, max, token)?,
        );
        if start > end {
            return Err(format!(
                "cannot translate \"{token}\" to OnCalendar: range start exceeds end"
            ));
        }
        format!("{}..{}", fmt(start), fmt(end))
    } else {
        fmt(parse_in_range(base, min, max, token)?)
    };
    match step {
        None => Ok(rendered),
        Some(step) => {
            // OnCalendar steps apply to `*` or a range, never to a single
            // value — reject rather than emit an invalid unit.
            if base != "*" && !base.contains('-') {
                return Err(format!(
                    "cannot translate \"{token}\" to OnCalendar: steps apply to * or a range, not a single value"
                ));
            }
            let step: u32 = step
                .parse()
                .map_err(|_| format!("cannot translate \"{token}\" to OnCalendar: bad step"))?;
            if step == 0 || step > max - min + 1 {
                return Err(format!(
                    "cannot translate \"{token}\" to OnCalendar: step out of range"
                ));
            }
            Ok(format!("{rendered}/{step}"))
        }
    }
}

fn parse_in_range(text: &str, min: u32, max: u32, token: &str) -> Result<u32, String> {
    let n: u32 = text
        .parse()
        .map_err(|_| format!("cannot translate \"{token}\" to OnCalendar: not a number"))?;
    if !(min..=max).contains(&n) {
        return Err(format!(
            "cannot translate \"{token}\" to OnCalendar: out of range {min}..{max}"
        ));
    }
    Ok(n)
}

const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Translate the month field: numbers 1-12 or three-letter names.
fn translate_month_field(field: &str) -> Result<String, String> {
    let tokens: Vec<&str> = field.split(',').collect();
    let mut out: Vec<String> = Vec::new();
    for token in tokens {
        let (base, step) = match token.split_once('/') {
            Some((base, step)) => (base, Some(step)),
            None => (token, None),
        };
        let rendered = if base == "*" {
            "*".to_string()
        } else if let Some((start, end)) = base.split_once('-') {
            format!(
                "{}..{}",
                month_token(start, token)?,
                month_token(end, token)?
            )
        } else {
            month_token(base, token)?
        };
        match step {
            None => out.push(rendered),
            Some(step) => {
                if base != "*" && !base.contains('-') {
                    return Err(format!(
                        "cannot translate \"{token}\" to OnCalendar: steps apply to * or a range, not a single value"
                    ));
                }
                let step: u32 = step
                    .parse()
                    .map_err(|_| format!("cannot translate \"{token}\" to OnCalendar: bad step"))?;
                if step == 0 || step > 12 {
                    return Err(format!(
                        "cannot translate \"{token}\" to OnCalendar: step out of range"
                    ));
                }
                out.push(format!("{rendered}/{step}"));
            }
        }
    }
    Ok(out.join(","))
}

fn month_token(text: &str, token: &str) -> Result<String, String> {
    if let Ok(n) = text.parse::<u32>() {
        if (1..=12).contains(&n) {
            return Ok(format!("{n:02}"));
        }
        return Err(format!(
            "cannot translate \"{token}\" to OnCalendar: month out of range 1..12"
        ));
    }
    let lower = text.to_ascii_lowercase();
    for name in MONTH_NAMES {
        if name.to_ascii_lowercase() == lower {
            return Ok(name.to_string());
        }
    }
    Err(format!(
        "cannot translate \"{token}\" to OnCalendar: unknown month name"
    ))
}

const DOW_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Translate the day-of-week field: numbers 1-7 (Sunday=1 — the crate's
/// range, recorded 2026-09-07: classic cron's 0=Sunday is rejected) or
/// three-letter names.
fn translate_dow_field(field: &str) -> Result<String, String> {
    let tokens: Vec<&str> = field.split(',').collect();
    let mut out: Vec<String> = Vec::new();
    for token in tokens {
        if token == "?" {
            out.push("*".to_string());
            continue;
        }
        if token.contains('?') {
            return Err(format!(
                "cannot translate \"{token}\" to OnCalendar: \"?\" must stand alone"
            ));
        }
        let (base, step) = match token.split_once('/') {
            Some((base, step)) => (base, Some(step)),
            None => (token, None),
        };
        let rendered = if base == "*" {
            "*".to_string()
        } else if let Some((start, end)) = base.split_once('-') {
            format!("{}..{}", dow_token(start, token)?, dow_token(end, token)?)
        } else {
            dow_token(base, token)?
        };
        match step {
            None => out.push(rendered),
            Some(step) => {
                if base != "*" && !base.contains('-') {
                    return Err(format!(
                        "cannot translate \"{token}\" to OnCalendar: steps apply to * or a range, not a single value"
                    ));
                }
                let step: u32 = step
                    .parse()
                    .map_err(|_| format!("cannot translate \"{token}\" to OnCalendar: bad step"))?;
                if step == 0 || step > 7 {
                    return Err(format!(
                        "cannot translate \"{token}\" to OnCalendar: step out of range"
                    ));
                }
                out.push(format!("{rendered}/{step}"));
            }
        }
    }
    Ok(out.join(","))
}

fn dow_token(text: &str, token: &str) -> Result<String, String> {
    if let Ok(n) = text.parse::<u32>() {
        return match n {
            1 => Ok("Sun".to_string()),
            2 => Ok("Mon".to_string()),
            3 => Ok("Tue".to_string()),
            4 => Ok("Wed".to_string()),
            5 => Ok("Thu".to_string()),
            6 => Ok("Fri".to_string()),
            7 => Ok("Sat".to_string()),
            _ => Err(format!(
                "cannot translate \"{token}\" to OnCalendar: weekday out of range 1..7"
            )),
        };
    }
    let lower = text.to_ascii_lowercase();
    for name in DOW_NAMES {
        if name.to_ascii_lowercase() == lower {
            return Ok(name.to_string());
        }
    }
    Err(format!(
        "cannot translate \"{token}\" to OnCalendar: unknown weekday name"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).expect("rfc3339").to_utc()
    }

    #[test]
    fn five_field_expressions_parse() {
        for expr in [
            "0 3 * * *",
            "*/15 * * * *",
            "0 9 * * mon-fri",
            "0 0 1,15 * *",
            "30 2 * 12 sun",
        ] {
            parse_schedule(expr).unwrap_or_else(|e| panic!("{expr}: {e}"));
        }
    }

    #[test]
    fn wrong_field_count_is_rejected() {
        assert!(parse_schedule("0 3 * *").is_err());
        assert!(parse_schedule("0 0 3 * * *").is_err());
        assert!(parse_schedule("* * * * * * *").is_err());
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse_schedule("sixty 3 * * *").is_err());
        assert!(parse_schedule("").is_err());
    }

    #[test]
    fn next_after_is_exclusive_and_returns_the_next_occurrence() {
        // "0 3 * * *" — daily at 03:00 UTC.
        let schedule = parse_schedule("0 3 * * *").expect("parse");
        // At exactly 03:00 the next occurrence is tomorrow (exclusive).
        let next = next_after(&schedule, at("2026-09-07T03:00:00Z")).expect("next");
        assert_eq!(next, at("2026-09-08T03:00:00Z"));
        // One second before 03:00 the next occurrence is today.
        let next = next_after(&schedule, at("2026-09-07T02:59:59Z")).expect("next");
        assert_eq!(next, at("2026-09-07T03:00:00Z"));
    }

    #[test]
    fn both_restricted_day_fields_are_rejected() {
        let err = parse_schedule("0 9 13 * 6").expect_err("both restricted");
        assert!(err.contains("both day-of-month and day-of-week"), "{err}");
        // Lists/ranges count as restricted too.
        assert!(parse_schedule("0 9 1-15 * 6").is_err());
        // One field restricted, the other * — accepted.
        assert!(parse_schedule("0 9 13 * *").is_ok());
        assert!(parse_schedule("0 9 * * 6").is_ok());
        // ? (unspecified) is not a restriction.
        assert!(parse_schedule("0 9 13 * ?").is_ok());
    }

    #[test]
    fn dom_and_dow_evaluate_independently() {
        // dom-restricted: fires on the 13th regardless of weekday.
        let dom = parse_schedule("0 9 13 * *").expect("parse");
        assert!(includes(&dom, at("2026-09-13T09:00:00Z")), "the 13th");
        assert!(!includes(&dom, at("2026-09-12T09:00:00Z")), "not the 13th");
        // dow-restricted: fires on Fridays (the crate's range is 1..=7
        // with Sunday=1, so Friday is 6).
        let dow = parse_schedule("0 9 * * 6").expect("parse");
        assert!(includes(&dow, at("2026-09-11T09:00:00Z")), "Friday 11th");
        assert!(!includes(&dow, at("2026-09-12T09:00:00Z")), "Saturday 12th");
    }

    #[test]
    fn on_calendar_translations() {
        assert_eq!(to_on_calendar("0 3 * * *").expect("t"), "*-*-* 3:0:00");
        assert_eq!(to_on_calendar("15 2 * * *").expect("t"), "*-*-* 2:15:00");
        assert_eq!(
            to_on_calendar("0 9 * * 2-6").expect("t"),
            "Mon..Fri *-*-* 9:0:00"
        );
        assert_eq!(
            to_on_calendar("0 0 1,15 * *").expect("t"),
            "*-*-01,15 0:0:00"
        );
        assert_eq!(to_on_calendar("0 9 13 * *").expect("t"), "*-*-13 9:0:00");
        assert_eq!(
            to_on_calendar("*/15 * * * *").expect("t"),
            "*-*-* *:*/15:00"
        );
        assert_eq!(
            to_on_calendar("30 2 * 12 sun").expect("t"),
            "Sun *-12-* 2:30:00"
        );
        assert_eq!(
            to_on_calendar("0 0 * jan-jun *").expect("t"),
            "*-Jan..Jun-* 0:0:00"
        );
    }

    #[test]
    fn on_calendar_rejects_untranslatable_shapes() {
        // A step over a single value is not expressible in OnCalendar.
        assert!(to_on_calendar("0 3 1,15/2 * *").is_err());
        assert!(to_on_calendar("0 3 * * 5/2").is_err());
    }

    #[test]
    fn on_calendar_range_steps_translate() {
        assert_eq!(
            to_on_calendar("0 3 * * 2-6/2").expect("t"),
            "Mon..Fri/2 *-*-* 3:0:00"
        );
    }

    #[test]
    fn sunday_mapping_and_zero_rejection() {
        assert_eq!(to_on_calendar("0 3 * * 7").expect("t"), "Sat *-*-* 3:0:00");
        assert_eq!(to_on_calendar("0 3 * * 1").expect("t"), "Sun *-*-* 3:0:00");
        // Classic cron's 0=Sunday is rejected by the crate (its range is
        // 1..=7) — the error must name the expression, not crash.
        assert!(parse_schedule("0 3 * * 0").is_err());
    }
}
