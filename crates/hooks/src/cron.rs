use chrono::{DateTime, Utc};
use std::str::FromStr;

/// Convert a 5-field cron expression (POSIX `minute hour dom month dow`)
/// to the 7-field format expected by the `cron` crate
/// (`second minute hour dom month dow [year]`).
///
/// The POSIX dow field uses `0-6` (or `0-7`) where `0/7 = Sunday`.
/// The `cron` crate uses `1-7` where `1 = Sunday` (i.e. each value is +1).
/// We translate numeric-only tokens; named tokens (Mon, Tue, …) pass through.
fn to_cron_expr(five_field: &str) -> Result<String, String> {
    let parts: Vec<&str> = five_field.split_whitespace().collect();
    if parts.len() != 5 {
        return Err(format!(
            "invalid cron schedule: expected 5 fields, got {}",
            parts.len()
        ));
    }
    let (minute, hour, dom, month, dow_posix) =
        (parts[0], parts[1], parts[2], parts[3], parts[4]);

    // Convert the DOW field from POSIX (0=Sun) to cron-crate (1=Sun) by
    // incrementing each numeric token by 1. Named tokens pass through unchanged.
    let dow_crate = translate_dow(dow_posix)?;

    Ok(format!("0 {minute} {hour} {dom} {month} {dow_crate}"))
}

/// Translate a POSIX DOW field token-by-token.
/// POSIX: 0=Sun, 1=Mon, …, 6=Sat (7 also treated as Sun in many crons).
/// cron-crate: 1=Sun, 2=Mon, …, 7=Sat.
fn translate_dow(posix_dow: &str) -> Result<String, String> {
    if posix_dow == "*" || posix_dow == "?" {
        return Ok(posix_dow.to_string());
    }

    // Split on commas to handle lists like "1,3,5"
    let mut translated_parts = Vec::new();
    for part in posix_dow.split(',') {
        translated_parts.push(translate_dow_part(part)?);
    }
    Ok(translated_parts.join(","))
}

/// Translate a single DOW part which may contain `/step` and `-range` notation.
fn translate_dow_part(part: &str) -> Result<String, String> {
    // Handle step notation: "1-5/2"
    if let Some((range, step)) = part.split_once('/') {
        let translated_range = translate_dow_range(range)?;
        return Ok(format!("{translated_range}/{step}"));
    }
    translate_dow_range(part)
}

/// Translate a DOW range like "1-5" or a single value "1" or a named value "Mon".
fn translate_dow_range(range: &str) -> Result<String, String> {
    if let Some((start, end)) = range.split_once('-') {
        let s = translate_dow_token(start)?;
        let e = translate_dow_token(end)?;
        Ok(format!("{s}-{e}"))
    } else {
        translate_dow_token(range)
    }
}

/// Translate a single DOW token: numeric 0-7 → +1, named (Mon etc.) unchanged.
fn translate_dow_token(token: &str) -> Result<String, String> {
    // If it looks like a number, add 1 (with wrapping: 7 → 1 like Sunday)
    if let Ok(n) = token.parse::<u8>() {
        // POSIX 0 and 7 both mean Sunday → cron-crate 1
        let translated = if n == 0 || n == 7 { 1u8 } else { n + 1 };
        if translated > 7 {
            return Err(format!("invalid cron DOW value: {token}"));
        }
        Ok(translated.to_string())
    } else {
        // Named token (Mon, Tue, etc.) — pass through
        Ok(token.to_string())
    }
}

/// Compute the next fire time for `schedule` (a 5-field cron expression:
/// minute hour dom month dow) strictly after `after`.
///
/// Returns `Err` if the expression cannot be parsed.
///
/// # Errors
///
/// Returns an error string if the cron expression cannot be parsed.
pub fn next_after(schedule: &str, after: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let expr_str = to_cron_expr(schedule)?;
    let schedule_parsed = cron::Schedule::from_str(&expr_str)
        .map_err(|e| format!("invalid cron schedule: {e}"))?;

    schedule_parsed
        .after(&after)
        .next()
        .ok_or_else(|| "cron schedule produces no upcoming events".to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn next_after_monday_9am_before_fires_this_week() {
        // "0 9 * * 1" = 9:00 every Monday
        // Given: 2024-01-15 08:59:00 UTC (a Monday)
        // next_after should return 2024-01-15 09:00:00 UTC
        let t = Utc.with_ymd_and_hms(2024, 1, 15, 8, 59, 0).unwrap();
        let next = next_after("0 9 * * 1", t).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 0).unwrap());
    }

    #[test]
    fn next_after_monday_9am_after_fires_next_week() {
        // Given: 2024-01-15 09:01:00 UTC (1 minute past 9am Monday)
        // next_after should return the NEXT Monday at 9am (2024-01-22 09:00:00 UTC)
        let t = Utc.with_ymd_and_hms(2024, 1, 15, 9, 1, 0).unwrap();
        let next = next_after("0 9 * * 1", t).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2024, 1, 22, 9, 0, 0).unwrap());
    }

    #[test]
    fn next_after_every_minute_fires_at_next_minute() {
        // "* * * * *" fires every minute
        // Given: 2024-01-15 09:00:30 UTC (30 sec past the minute)
        // Should fire at the NEXT minute: 2024-01-15 09:01:00 UTC
        let t = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 30).unwrap();
        let next = next_after("* * * * *", t).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2024, 1, 15, 9, 1, 0).unwrap());
    }

    #[test]
    fn next_after_invalid_expression_returns_error() {
        let t = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 0).unwrap();
        let result = next_after("not-a-cron", t);
        assert!(result.is_err(), "invalid cron should return Err");
        let err = result.unwrap_err();
        assert!(
            err.contains("invalid cron schedule"),
            "error should mention 'invalid cron schedule', got: {err}"
        );
    }

    #[test]
    fn next_after_every_hour_at_minute_0() {
        // "0 * * * *" = top of every hour
        // Given: 2024-01-15 09:45:00 UTC
        // Should fire at 2024-01-15 10:00:00 UTC
        let t = Utc.with_ymd_and_hms(2024, 1, 15, 9, 45, 0).unwrap();
        let next = next_after("0 * * * *", t).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2024, 1, 15, 10, 0, 0).unwrap());
    }

    #[test]
    fn next_after_daily_midnight() {
        // "0 0 * * *" = midnight every day
        // Given: 2024-01-15 12:00:00 UTC
        // Should fire at 2024-01-16 00:00:00 UTC
        let t = Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap();
        let next = next_after("0 0 * * *", t).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2024, 1, 16, 0, 0, 0).unwrap());
    }

    #[test]
    fn next_after_at_exact_fire_time_moves_to_next_occurrence() {
        // If we ask for the next fire "after" 2024-01-15 09:00:00 exactly,
        // we should NOT get 09:00:00 back — the `after` is exclusive.
        let t = Utc.with_ymd_and_hms(2024, 1, 15, 9, 0, 0).unwrap();
        let next = next_after("0 9 * * 1", t).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2024, 1, 22, 9, 0, 0).unwrap());
    }

    #[test]
    fn translate_dow_star_passes_through() {
        assert_eq!(translate_dow("*").unwrap(), "*");
    }

    #[test]
    fn translate_dow_zero_to_one() {
        // POSIX 0 = Sunday → crate 1
        assert_eq!(translate_dow("0").unwrap(), "1");
    }

    #[test]
    fn translate_dow_one_to_two() {
        // POSIX 1 = Monday → crate 2
        assert_eq!(translate_dow("1").unwrap(), "2");
    }

    #[test]
    fn translate_dow_seven_to_one() {
        // POSIX 7 = Sunday (alias) → crate 1
        assert_eq!(translate_dow("7").unwrap(), "1");
    }

    #[test]
    fn translate_dow_named_passes_through() {
        assert_eq!(translate_dow("Mon").unwrap(), "Mon");
        assert_eq!(translate_dow("Fri").unwrap(), "Fri");
    }

    #[test]
    fn translate_dow_range() {
        // POSIX "1-5" (Mon-Fri) → crate "2-6"
        assert_eq!(translate_dow("1-5").unwrap(), "2-6");
    }

    #[test]
    fn translate_dow_list() {
        // POSIX "1,3,5" (Mon,Wed,Fri) → crate "2,4,6"
        assert_eq!(translate_dow("1,3,5").unwrap(), "2,4,6");
    }

    #[test]
    fn translate_dow_question_mark_passes_through() {
        assert_eq!(translate_dow("?").unwrap(), "?");
    }

    #[test]
    fn translate_dow_step_notation_1_5_step_2() {
        // POSIX "1-5/2" (Mon-Fri, every 2nd) → crate "2-6/2"
        assert_eq!(translate_dow("1-5/2").unwrap(), "2-6/2");
    }

    #[test]
    fn translate_dow_token_rejects_value_above_7() {
        let result = translate_dow("8");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("invalid cron DOW value"), "got: {msg}");
    }
}
