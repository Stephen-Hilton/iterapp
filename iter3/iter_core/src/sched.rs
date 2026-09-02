//! Scheduled workitems — the V2 itersched model ported to V3 (itersched.md).
//!
//! A schedule is an ordinary workitem parked in `state: "scheduled"` — a
//! TEMPLATE the picker never takes. When due, the engine CLONES it into a
//! normal queued run (fresh id, `source_schedule` provenance, priority
//! inherited). Rules are engine-owned: dedup while any clone is open;
//! skip-don't-backfill for daily/weekly; users-only creation (iter_data
//! rejects schedules from the engine role — the agents' path).

use chrono::{DateTime, Datelike, Duration, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Sched {
    /// every | daily | weekly | stale
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub every_min: u64,
    /// "HH:MM" for daily/weekly
    #[serde(default)]
    pub at: String,
    /// mon..sun for weekly
    #[serde(default)]
    pub day: String,
    /// IANA zone; empty = UTC (engine math is always UTC; local clocks opt in)
    #[serde(default)]
    pub tz: String,
    /// restart memory: last time this template fired (ISO UTC)
    #[serde(default)]
    pub last_fired: String,
}

/// Skip-don't-backfill window for daily/weekly: an occurrence older than this
/// happened while the engine was down — skip to the next natural time.
pub const OCCURRENCE_WINDOW_SEC: i64 = 150;

pub fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

fn tz_of(sched: &Sched) -> Tz {
    sched.tz.trim().parse().unwrap_or(chrono_tz::UTC)
}

fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    let (h, m) = s.trim().split_once(':')?;
    NaiveTime::from_hms_opt(h.parse().ok()?, m.parse().ok()?, 0)
}

fn parse_day(s: &str) -> Option<Weekday> {
    match s.trim().to_lowercase().get(..3)? {
        "mon" => Some(Weekday::Mon),
        "tue" => Some(Weekday::Tue),
        "wed" => Some(Weekday::Wed),
        "thu" => Some(Weekday::Thu),
        "fri" => Some(Weekday::Fri),
        "sat" => Some(Weekday::Sat),
        "sun" => Some(Weekday::Sun),
        _ => None,
    }
}

/// Most recent occurrence of "at HH:MM in tz [on weekday]" at or before `now`.
/// Walks back day by day, so a DST gap resolves to the previous valid one.
fn last_occurrence(
    now: DateTime<Utc>,
    tz: Tz,
    at: NaiveTime,
    day: Option<Weekday>,
) -> Option<DateTime<Utc>> {
    let mut date = now.with_timezone(&tz).date_naive();
    for _ in 0..9 {
        if day.map(|d| date.weekday() == d).unwrap_or(true) {
            if let Some(occ) = tz.from_local_datetime(&date.and_time(at)).earliest() {
                let occ = occ.with_timezone(&Utc);
                if occ <= now {
                    return Some(occ);
                }
            }
        }
        date = date.pred_opt()?;
    }
    None
}

/// Is this schedule due at `now`? `added` = the template's receive time;
/// `last_completed` = newest completion among clones (the "stale" kind's real
/// question is "when did this last FINISH", not "when did I last try").
pub fn due(
    sched: &Sched,
    added: &str,
    now: DateTime<Utc>,
    last_completed: Option<DateTime<Utc>>,
) -> bool {
    let last_fired = parse_iso(&sched.last_fired);
    match sched.kind.as_str() {
        "every" => {
            if sched.every_min == 0 {
                return false;
            }
            let anchor = last_fired.or_else(|| parse_iso(added)).unwrap_or(now);
            now >= anchor + Duration::minutes(sched.every_min as i64)
        }
        "stale" => {
            if sched.every_min == 0 {
                return false;
            }
            // anchor on the newest signal: completion, fire (covers a clone
            // that failed terminally — no rapid-fire loop), or creation
            let anchor = [last_completed, last_fired, parse_iso(added)]
                .into_iter()
                .flatten()
                .max()
                .unwrap_or(now);
            now >= anchor + Duration::minutes(sched.every_min as i64)
        }
        "daily" | "weekly" => {
            let Some(at) = parse_hhmm(&sched.at) else { return false };
            let day = if sched.kind == "weekly" {
                match parse_day(&sched.day) {
                    Some(d) => Some(d),
                    None => return false,
                }
            } else {
                None
            };
            let Some(occ) = last_occurrence(now, tz_of(sched), at, day) else { return false };
            if (now - occ).num_seconds() > OCCURRENCE_WINDOW_SEC {
                return false; // missed while down — skip, never backfill
            }
            last_fired.map(|lf| lf < occ).unwrap_or(true)
        }
        _ => false,
    }
}

/// The queued run a firing template produces: same work, fresh identity and
/// lifecycle, provenance via source_schedule, priority inherited.
pub fn clone_from(template: &crate::WorkItem) -> crate::WorkItem {
    let mut clone = template.clone();
    clone.id = String::new(); // iter_data assigns
    clone.version = 0;
    clone.state = "queued".into();
    clone.sched = None;
    clone.source_schedule = template.id.clone();
    clone.createdby = "scheduler".into();
    clone.attempt = 0;
    clone.engine = String::new();
    clone.lasterror = String::new();
    clone.ts = crate::WorkItemTs::default();
    clone
}

/// Clone states that hold the dedup gate closed: while any clone is in one of
/// these, the template does not fire (failed-and-retrying rides "queued").
pub fn is_open_state(state: &str) -> bool {
    matches!(state, "queued" | "in-progress" | "question" | "paused")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sched(kind: &str) -> Sched {
        Sched { kind: kind.into(), every_min: 60, ..Default::default() }
    }

    #[test]
    fn every_fires_on_interval_from_anchor() {
        let now = parse_iso("2026-09-01T12:00:00Z").unwrap();
        let mut s = sched("every");
        // never fired: anchor = added
        assert!(due(&s, "2026-09-01T10:00:00Z", now, None));
        assert!(!due(&s, "2026-09-01T11:30:00Z", now, None));
        // fired recently: not due
        s.last_fired = "2026-09-01T11:30:00Z".into();
        assert!(!due(&s, "2026-09-01T10:00:00Z", now, None));
        s.last_fired = "2026-09-01T10:59:00Z".into();
        assert!(due(&s, "2026-09-01T10:00:00Z", now, None));
    }

    #[test]
    fn daily_fires_in_window_and_skips_missed() {
        let mut s = sched("daily");
        s.at = "12:00".into();
        let now = parse_iso("2026-09-01T12:01:00Z").unwrap();
        assert!(due(&s, "2026-08-01T00:00:00Z", now, None));
        // 10 minutes late = missed while down: skip
        let late = parse_iso("2026-09-01T12:10:00Z").unwrap();
        assert!(!due(&s, "2026-08-01T00:00:00Z", late, None));
        // already fired for this occurrence
        s.last_fired = "2026-09-01T12:00:30Z".into();
        assert!(!due(&s, "2026-08-01T00:00:00Z", now, None));
    }

    #[test]
    fn weekly_needs_matching_day() {
        let mut s = sched("weekly");
        s.at = "12:00".into();
        s.day = "tue".into();
        // 2026-09-01 is a Tuesday
        let now = parse_iso("2026-09-01T12:01:00Z").unwrap();
        assert!(due(&s, "2026-08-01T00:00:00Z", now, None));
        s.day = "wed".into();
        assert!(!due(&s, "2026-08-01T00:00:00Z", now, None));
    }

    #[test]
    fn stale_anchors_on_newest_signal() {
        let s = sched("stale");
        let now = parse_iso("2026-09-01T12:00:00Z").unwrap();
        // completed recently: not due
        assert!(!due(&s, "2026-09-01T00:00:00Z", now, parse_iso("2026-09-01T11:30:00Z")));
        // completed long ago: due
        assert!(due(&s, "2026-09-01T00:00:00Z", now, parse_iso("2026-09-01T10:00:00Z")));
    }

    #[test]
    fn clone_resets_lifecycle() {
        let mut t = crate::WorkItem::default();
        t.id = "tpl-1".into();
        t.state = "scheduled".into();
        t.priority = 8;
        t.sched = Some(sched("every"));
        t.attempt = 3;
        let c = clone_from(&t);
        assert_eq!(c.state, "queued");
        assert_eq!(c.source_schedule, "tpl-1");
        assert_eq!(c.priority, 8);
        assert_eq!(c.attempt, 0);
        assert!(c.sched.is_none() && c.id.is_empty());
    }
}
