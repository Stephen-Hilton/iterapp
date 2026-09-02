//! itersched — fires `scheduled` work items (templates) into the queue.
//!
//! A schedule is an ordinary work item held in `state: "scheduled"`: the loop
//! never picks it (see `WorkItem::eligible`); every `CHECK_INTERVAL_SEC` the
//! engine asks whether it is due and, if so, CLONES it into a normal queued
//! run. The clone carries `source_schedule: <template workid>` — the dedup
//! key: while any clone of a schedule is still open (queued, in-progress, or
//! failed-awaiting-retry), the schedule never fires again, so duplicates are
//! impossible. The template's `sched.last_fired` (persisted on the work item's
//! own row) is the restart memory; the `sched_log` table is the append-only
//! audit trail — `iter export --table sched_log` dumps it back to jsonl.
//!
//! Missed clock-anchored occurrences (daily/weekly) while the engine was down
//! are SKIPPED, never backfired (user decision 2026-08-17): an occurrence only
//! fires within `OCCURRENCE_WINDOW_SEC` of its scheduled minute. Interval
//! kinds (every/stale) have no fixed occurrences — firing when overdue IS
//! their normal operation, and dedup caps them at one fire.
//!
//! Only users create schedules: the webapp API accepts `state: "scheduled"`,
//! `iter add` (the agents' path) refuses it.

use chrono::{DateTime, Datelike, Duration, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use std::path::Path;

use crate::config::Config;
use crate::logging;
use crate::workitems::{self, Queue, Sched, WorkItem, STATE_SCHEDULED};

/// Check cadence — minute granularity by design; no scheduling to the second.
pub const CHECK_INTERVAL_SEC: u64 = 59;

/// Skip-don't-backfill window for daily/weekly: an occurrence older than this
/// happened while the engine was down — skip to the next natural time. The 59s
/// check cadence cannot miss a live occurrence inside 150s.
const OCCURRENCE_WINDOW_SEC: i64 = 150;

/// A schedule's own tz, or UTC. Deliberately NOT user_timezone: since the
/// 2026-08-27 audit user_timezone is display-only — everything the ENGINE
/// computes runs in UTC, and a schedule that wants a local clock says so with
/// its own tz field.
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
/// Walks back day by day, so a DST gap (a local time that never existed) simply
/// resolves to the previous valid occurrence.
fn last_occurrence(now: DateTime<Utc>, tz: Tz, at: NaiveTime, day: Option<Weekday>) -> Option<DateTime<Utc>> {
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

/// Is this schedule due at `now`? `last_completed` = newest `times.closed`
/// among COMPLETED clones (the "stale" kind's real question is "when did this
/// last finish", not "when did I last try").
pub fn due(
    sched: &Sched,
    added: &str,
    now: DateTime<Utc>,
    last_completed: Option<DateTime<Utc>>,
) -> bool {
    let last_fired = workitems::parse_iso(&sched.last_fired);
    match sched.kind.as_str() {
        "every" => {
            if sched.every_min == 0 {
                return false;
            }
            let anchor = last_fired.or_else(|| workitems::parse_iso(added)).unwrap_or(now);
            now >= anchor + Duration::minutes(sched.every_min as i64)
        }
        "stale" => {
            if sched.every_min == 0 {
                return false;
            }
            // Anchor on the newest signal we have: a completion, a fire (covers
            // a clone that failed terminally — no rapid-fire loop), or creation.
            let anchor = [last_completed, last_fired, workitems::parse_iso(added)]
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

/// The queued run a firing schedule produces: same work, fresh identity and
/// lifecycle, provenance via `source_schedule`, priority inherited from the
/// template (the per-schedule "how urgent are my runs" knob).
pub fn clone_from(parent: &WorkItem, now_iso: &str) -> WorkItem {
    let mut c = parent.clone();
    c.workid = uuid::Uuid::new_v4().to_string();
    c.state = workitems::STATE_QUEUED.into();
    c.source = "scheduler".into();
    c.source_schedule = parent.workid.clone();
    c.sched = Sched::default();
    // Schedules are cadence-driven: a dependency gate on a recurring run would
    // invite a silent never-runs, so clones never carry one (the API refuses
    // depends_on on templates; this covers hand-edited queues too).
    c.depends_on = Vec::new();
    c.depends_on_shallow = false;
    c.attempts = 0;
    c.output = String::new();
    c.lasterror = String::new();
    c.times = workitems::Times { added: now_iso.to_string(), ..Default::default() };
    c
}

/// Append one fire to the audit trail (the `sched_log` table). The record is
/// stored as the same JSON object the file form held on one line, so
/// `iter export --table sched_log` still produces the trail people already know
/// how to read; only `ts` is lifted into a column, to order and window it.
///
/// Best-effort, like the append it replaces: a schedule that fired must not be
/// reported as failing because its audit line could not be written.
fn log_fire(project_root: &Path, parent: &WorkItem, clone_id: &str, reason: &str) {
    let ts = workitems::now_iso();
    let line = serde_json::json!({
        "ts": ts,
        "schedule": parent.workid,
        "title": parent.title,
        "kind": parent.sched.kind,
        "clone": clone_id,
        "reason": reason,
    });
    if let Ok(mut conn) = crate::db::open(project_root) {
        // Carry any pre-database trail in before appending, so the log stays one
        // continuous history rather than restarting at the upgrade.
        let _ = crate::db::import_sched_log_jsonl(&mut conn, project_root);
        let _ = crate::db::record_sched(&conn, &ts, &line.to_string());
    }
}

/// Clone a schedule into a queued run, under the record lock. Dedup happens
/// INSIDE the lock: one open clone per schedule, ever. Returns Ok(None) when
/// suppressed (an open clone exists, or the queue is at max_open_workitems).
/// Also the engine-owned semantics of the API's "queue" action on a scheduled
/// item: queueing a schedule MEANS clone-and-queue, never queueing the
/// template itself.
pub fn fire(
    project_root: &Path,
    cfg: &Config,
    schedule_id: &str,
    reason: &str,
) -> Result<Option<String>, String> {
    let queue = Queue::new(project_root, cfg);
    let now_iso = workitems::now_iso();
    let cap = cfg.engine.max_open_workitems;
    let outcome = queue
        .with_lock(|items| {
            let Some(pos) = items
                .iter()
                .position(|i| i.workid == schedule_id && i.state == STATE_SCHEDULED)
            else {
                return Err("no such scheduled item".to_string());
            };
            if items.iter().any(|i| i.source_schedule == schedule_id) {
                return Ok(None); // an earlier run is still open: no duplicates
            }
            if items.len() >= cap {
                return Err(format!("queue at max_open_workitems ({})", cap));
            }
            let clone = clone_from(&items[pos], &now_iso);
            let id = clone.workid.clone();
            items[pos].sched.last_fired = now_iso.clone();
            let parent = items[pos].clone();
            items.push(clone);
            Ok(Some((parent, id)))
        })
        .map_err(|e| e.to_string())?;
    match outcome {
        Ok(Some((parent, id))) => {
            logging::info(
                "sched",
                &format!("schedule \"{}\" fired ({}) → queued {}", parent.title, reason, &id[..8.min(id.len())]),
            );
            log_fire(project_root, &parent, &id, reason);
            Ok(Some(id))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(e),
    }
}

/// One itersched pass: fire every due schedule. Called from the engine loop on
/// the `CHECK_INTERVAL_SEC` cadence (with the queue mutex held). Returns the
/// fired clone ids.
pub fn check(project_root: &Path, cfg: &Config) -> Vec<String> {
    let queue = Queue::new(project_root, cfg);
    let now = Utc::now();
    let items = queue.load();
    let schedules: Vec<WorkItem> = items
        .iter()
        .filter(|i| i.state == STATE_SCHEDULED && !i.sched.is_none())
        .cloned()
        .collect();
    if schedules.is_empty() {
        return Vec::new();
    }
    let closed = if schedules.iter().any(|s| s.sched.kind == "stale") {
        queue.load_closed()
    } else {
        Vec::new()
    };
    let mut fired = Vec::new();
    for parent in &schedules {
        if items.iter().any(|i| i.source_schedule == parent.workid) {
            continue; // open clone → suppressed (fire() re-checks under the lock)
        }
        let last_completed = closed
            .iter()
            .filter(|c| c.source_schedule == parent.workid && c.state == workitems::STATE_COMPLETE)
            .filter_map(|c| workitems::parse_iso(&c.times.closed))
            .max();
        if !due(&parent.sched, &parent.times.added, now, last_completed) {
            continue;
        }
        match fire(project_root, cfg, &parent.workid, "due") {
            Ok(Some(id)) => fired.push(id),
            Ok(None) => {}
            Err(e) => logging::error("sched", &format!("fire failed for \"{}\": {}", parent.title, e)),
        }
    }
    fired
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sched(kind: &str) -> Sched {
        Sched { kind: kind.into(), ..Default::default() }
    }

    fn iso(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn every_fires_on_interval_from_anchor() {
        let now = Utc::now();
        let mut s = sched("every");
        s.every_min = 30;
        // never fired: anchor = added
        let added = (now - Duration::minutes(31)).to_rfc3339();
        assert!(due(&s, &added, now, None));
        let added = (now - Duration::minutes(10)).to_rfc3339();
        assert!(!due(&s, &added, now, None));
        // fired recently: not due
        s.last_fired = (now - Duration::minutes(10)).to_rfc3339();
        assert!(!due(&s, &iso(""), now, None));
        s.last_fired = (now - Duration::minutes(31)).to_rfc3339();
        assert!(due(&s, &iso(""), now, None));
        s.every_min = 0;
        assert!(!due(&s, &iso(""), now, None), "unset interval never fires");
    }

    #[test]
    fn stale_anchors_on_newest_of_completion_fire_or_creation() {
        let now = Utc::now();
        let mut s = sched("stale");
        s.every_min = 60;
        let old = (now - Duration::minutes(120)).to_rfc3339();
        // completed recently → not due, even though last fire is old
        s.last_fired = old.clone();
        assert!(!due(&s, &old, now, Some(now - Duration::minutes(5))));
        // completion old, fire old → due
        assert!(due(&s, &old, now, Some(now - Duration::minutes(90))));
        // fire recent (failed clone just closed) → not due: no rapid-fire loop
        s.last_fired = (now - Duration::minutes(5)).to_rfc3339();
        assert!(!due(&s, &old, now, Some(now - Duration::minutes(90))));
    }

    #[test]
    fn daily_fires_in_window_and_skips_missed() {
        let mut s = sched("daily");
        // "now" fixed at 10:00:30 UTC
        let now = Utc.with_ymd_and_hms(2026, 8, 17, 10, 0, 30).unwrap();
        s.at = "10:00".into();
        assert!(due(&s, "", now, None), "inside the occurrence window");
        s.last_fired = now.to_rfc3339();
        assert!(!due(&s, "", now, None), "already fired this occurrence");
        // engine was down: occurrence 3h old → skip, never backfill
        let mut missed = sched("daily");
        missed.at = "07:00".into();
        assert!(!due(&missed, "", now, None));
        // explicit tz: 10:00 America/Los_Angeles is 17:00/18:00 UTC, not now
        let mut la = sched("daily");
        la.at = "10:00".into();
        la.tz = "America/Los_Angeles".into();
        assert!(!due(&la, "", now, None));
    }

    #[test]
    fn weekly_needs_matching_day() {
        // 2026-08-16 is a Sunday; fix now at Sun 22:00:20 UTC
        let now = Utc.with_ymd_and_hms(2026, 8, 16, 22, 0, 20).unwrap();
        let mut s = sched("weekly");
        s.at = "22:00".into();
        s.day = "sunday".into();
        assert!(due(&s, "", now, None));
        s.day = "mon".into();
        assert!(!due(&s, "", now, None));
        s.day = "not-a-day".into();
        assert!(!due(&s, "", now, None));
    }

    #[test]
    fn fire_dedups_and_clones() {
        let dir = std::env::temp_dir().join(format!("itersched-fire-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".iter/.engine")).unwrap();
        let cfg = Config::default();
        let queue = Queue::new(&dir, &cfg);
        let mut tpl = WorkItem {
            workid: "sched-1".into(),
            title: "nightly cleanup".into(),
            item_type: "code".into(),
            state: STATE_SCHEDULED.into(),
            priority: 8,
            ..Default::default()
        };
        tpl.sched = Sched { kind: "every".into(), every_min: 5, ..Default::default() };
        queue.append(&tpl).unwrap();

        let id = fire(&dir, &cfg, "sched-1", "test").unwrap().expect("fires");
        let items = queue.load();
        assert_eq!(items.len(), 2);
        let clone = items.iter().find(|i| i.workid == id).unwrap();
        assert_eq!(clone.state, workitems::STATE_QUEUED);
        assert_eq!(clone.source, "scheduler");
        assert_eq!(clone.source_schedule, "sched-1");
        assert_eq!(clone.priority, 8, "clone priority inherits the template's");
        assert!(clone.sched.is_none(), "clones carry no schedule of their own");
        let parent = items.iter().find(|i| i.workid == "sched-1").unwrap();
        assert!(!parent.sched.last_fired.is_empty(), "last_fired persists on the template");

        // open clone suppresses the next fire
        assert!(fire(&dir, &cfg, "sched-1", "test").unwrap().is_none());
        // check() also suppresses (and the audit log recorded exactly one fire)
        assert!(check(&dir, &cfg).is_empty());
        let conn = crate::db::open(&dir).unwrap();
        let log = crate::db::export_table(&conn, "sched_log", crate::db::ExportScope::All).unwrap();
        assert_eq!(log.len(), 1, "{:?}", log);
        assert!(log[0].contains("sched-1"), "{}", log[0]);
        assert!(log[0].contains("nightly cleanup"), "the trail names the schedule: {}", log[0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A project upgraded mid-life keeps its audit trail: the pre-database
    /// `sched_log.jsonl` is imported once, the file is renamed rather than
    /// deleted, and the next fire appends to the same continuous history.
    #[test]
    fn existing_sched_log_imports_once_and_keeps_appending() {
        let dir = std::env::temp_dir().join(format!("itersched-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".iter/.engine")).unwrap();
        let path = dir.join(".iter/.engine/sched_log.jsonl");
        std::fs::write(
            &path,
            "{\"ts\":\"2026-08-01T00:00:00Z\",\"schedule\":\"old-1\",\"reason\":\"due\"}\n\
             {\"ts\":\"2026-08-02T00:00:00Z\",\"schedule\":\"old-2\",\"reason\":\"due\"}\n",
        )
        .unwrap();

        let cfg = Config::default();
        let queue = Queue::new(&dir, &cfg);
        let mut tpl = WorkItem {
            workid: "sched-9".into(),
            title: "after the upgrade".into(),
            item_type: "code".into(),
            state: STATE_SCHEDULED.into(),
            ..Default::default()
        };
        tpl.sched = Sched { kind: "every".into(), every_min: 5, ..Default::default() };
        queue.append(&tpl).unwrap();
        fire(&dir, &cfg, "sched-9", "test").unwrap().expect("fires");

        let conn = crate::db::open(&dir).unwrap();
        let log = crate::db::export_table(&conn, "sched_log", crate::db::ExportScope::All).unwrap();
        assert_eq!(log.len(), 3, "two imported plus one new: {:?}", log);
        assert!(log[0].contains("old-1"), "history comes first, in order");
        assert!(log[2].contains("sched-9"));
        // Exported verbatim: the imported lines are byte-identical to the file's.
        assert_eq!(log[0], "{\"ts\":\"2026-08-01T00:00:00Z\",\"schedule\":\"old-1\",\"reason\":\"due\"}");
        assert!(!path.exists());
        assert!(dir.join(".iter/.engine/sched_log.jsonl.imported").is_file());

        // Idempotent: a second fire must not re-import the (renamed) file.
        std::fs::write(&path, "{\"ts\":\"2026-08-03T00:00:00Z\",\"schedule\":\"decoy\"}\n").unwrap();
        log_fire(&dir, &tpl, "clone-2", "test");
        let log = crate::db::export_table(&conn, "sched_log", crate::db::ExportScope::All).unwrap();
        assert_eq!(log.len(), 4, "the decoy file is not imported into a populated table");
        assert!(!log.iter().any(|l| l.contains("decoy")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn _typecheck(_: PathBuf) {}
}
