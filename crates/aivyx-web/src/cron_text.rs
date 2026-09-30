//! Plain-English schedule text for the cron strings routines carry
//! (`sec min hour day-of-month month day-of-week [year]`). A newcomer
//! reading "0 0 7 * * *" on a routine row learns nothing; "every day at
//! 07:00" says it. Only the shapes the Studio's own builder and init's
//! default routines produce are described — anything else returns
//! `None` and the caller shows the raw cron.

const DAYS: [(&str, &str); 7] = [
    ("MON", "Monday"),
    ("TUE", "Tuesday"),
    ("WED", "Wednesday"),
    ("THU", "Thursday"),
    ("FRI", "Friday"),
    ("SAT", "Saturday"),
    ("SUN", "Sunday"),
];

fn day_name(s: &str) -> Option<&'static str> {
    // Numbered days follow the `cron` crate the scheduler uses:
    // Sunday = 1 … Saturday = 7 (so `1` is *not* Monday).
    if let Ok(n) = s.parse::<usize>() {
        return match n {
            1 => Some("Sunday"),
            2..=7 => Some(DAYS[n - 2].1),
            _ => None,
        };
    }
    let up = s.to_ascii_uppercase();
    let key = up.get(..3)?;
    DAYS.iter().find(|(k, _)| *k == key).map(|(_, n)| *n)
}

fn num(s: &str, max: u32) -> Option<u32> {
    s.parse::<u32>().ok().filter(|n| *n <= max)
}

fn every(s: &str) -> Option<u32> {
    s.strip_prefix("*/")?.parse::<u32>().ok().filter(|n| *n > 0)
}

/// Describe `cron` in words, or `None` when it isn't a shape this knows.
pub fn describe_cron(cron: &str) -> Option<String> {
    let f: Vec<&str> = cron.split_whitespace().collect();
    if !(f.len() == 6 || (f.len() == 7 && f[6] == "*")) {
        return None;
    }
    let (sec, min, hour, dom, mon, dow) = (f[0], f[1], f[2], f[3], f[4], f[5]);
    if sec != "0" || mon != "*" {
        return None;
    }

    // Sub-daily repeats, any day.
    if dom == "*" && dow == "*" {
        if let Some(n) = every(min) {
            if hour == "*" {
                return Some(if n == 1 {
                    "every minute".into()
                } else {
                    format!("every {n} minutes")
                });
            }
        }
        if min == "0" {
            if hour == "*" {
                return Some("every hour".into());
            }
            if let Some(n) = every(hour) {
                return Some(if n == 1 {
                    "every hour".into()
                } else {
                    format!("every {n} hours")
                });
            }
        }
    }

    let at = format!("{:02}:{:02}", num(hour, 23)?, num(min, 59)?);
    match (dom, dow) {
        ("*", "*") => Some(format!("every day at {at}")),
        ("*", d) => {
            let up = d.to_ascii_uppercase();
            if up == "MON-FRI" {
                return Some(format!("weekdays at {at}"));
            }
            if up == "SAT,SUN" || up == "SAT-SUN" {
                return Some(format!("weekends at {at}"));
            }
            Some(format!("every {} at {at}", day_name(d)?))
        }
        (d, "*") => Some(format!("on day {} of each month at {at}", num(d, 31)?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::describe_cron;

    #[test]
    fn daily_weekly_and_monthly_read_as_sentences() {
        let d = |c| describe_cron(c).unwrap_or_else(|| panic!("{c}"));
        assert_eq!(d("0 0 7 * * *"), "every day at 07:00");
        assert_eq!(d("0 30 9 * * * *"), "every day at 09:30");
        assert_eq!(d("0 0 9 * * Mon *"), "every Monday at 09:00");
        assert_eq!(d("0 5 18 * * FRI"), "every Friday at 18:05");
        assert_eq!(d("0 0 9 * * Mon-Fri"), "weekdays at 09:00");
        assert_eq!(d("0 0 10 * * Sat,Sun"), "weekends at 10:00");
        assert_eq!(d("0 0 3 1 * *"), "on day 1 of each month at 03:00");
        // The scheduler's `cron` crate counts from Sunday = 1.
        assert_eq!(d("0 0 8 * * 1"), "every Sunday at 08:00");
        assert_eq!(d("0 0 8 * * 2"), "every Monday at 08:00");
        assert_eq!(d("0 0 8 * * 7"), "every Saturday at 08:00");
    }

    #[test]
    fn repeats_within_a_day() {
        let d = |c| describe_cron(c).unwrap_or_else(|| panic!("{c}"));
        assert_eq!(d("0 0 * * * *"), "every hour");
        assert_eq!(d("0 0 */6 * * * *"), "every 6 hours");
        assert_eq!(d("0 */15 * * * *"), "every 15 minutes");
        assert_eq!(d("0 */1 * * * *"), "every minute");
    }

    #[test]
    fn anything_else_is_left_to_the_raw_cron() {
        for c in [
            "",
            "* * * * * *",
            "0 0 9 * * * 2027",
            "0 0 9 * 1 *",
            "0 0 25 * * *",
            "0 0 9 * * 0",
            "0 0 9 * * 8",
            "0 0 9 1 * Mon",
            "30 0 9 * * *",
            "0 0 9-17 * * *",
        ] {
            assert_eq!(describe_cron(c), None, "{c}");
        }
    }
}
