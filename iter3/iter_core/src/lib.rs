//! iter_core — shared types for iter V3 (spec: src/features/iter.v3.md).
//! These are the wire shapes both iter_data and iter_engine speak; storage
//! backends persist them as JSON bodies, so unknown fields round-trip.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod sched;
pub mod widget;

/// Logical table names — storage backends map these to physical names
/// (DynamoDB: prefix + name; SQLite: table name).
pub const TABLES: &[&str] = &[
    "agent",
    "project",
    "engine",
    "workitem",
    "workitem_detail",
    "project_prepostwork",
    "webui_user",
    "webui",
    "versions",
    "lock",
    "project_structure",
];

/// Workitem states (glossary in iter.v3.md).
pub const STATES: &[&str] = &[
    "in-progress",
    "queued",
    "question",
    "parked",
    "paused",
    "failed",
    "complete",
    "scheduled",
];

pub fn now_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentDef {
    pub name: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub childstate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeoutsec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flags: Option<String>,
    #[serde(default)]
    pub promptbody: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Account {
    pub name: String,
    pub token_envar: String,
    #[serde(default)]
    pub order: i64,
    /// switch to next account at this 5hr/7d usage % (first pass)
    #[serde(default)]
    pub switch: u8,
    /// hard stop for this account at this usage % (second pass)
    #[serde(default)]
    pub stop: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FailurePolicy {
    #[serde(default = "default_maxattempts")]
    pub maxattempts: u32,
    #[serde(default = "default_first_retry")]
    pub first_retry_second: u64,
    #[serde(default = "default_backoff")]
    pub retry_backoff_exponent: u32,
}
fn default_maxattempts() -> u32 { 5 }
fn default_first_retry() -> u64 { 10 }
fn default_backoff() -> u32 { 2 }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub desc: String,
    /// Aspirational: Running | Draining | Stopped
    #[serde(default = "default_running")]
    pub state: String,
    #[serde(default)]
    pub gitrepo: String,
    /// ordered usage-gates, e.g. {">98%": 0, "else": 4}
    #[serde(default)]
    pub maxagents: BTreeMap<String, u32>,
    /// null/absent = unlimited; 0 = spend nothing; >0 = $/day cap
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maxdailycost: Option<f64>,
    /// per-project agent overrides keyed by agent name
    #[serde(default)]
    pub agents: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub failure: FailurePolicy,
    /// links to iter3_engine records by name
    #[serde(default)]
    pub engines: Vec<String>,
    #[serde(default)]
    pub accounts: Vec<Account>,
}
fn default_running() -> String { "Running".into() }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EngineProjectDirs {
    #[serde(default)]
    pub dirs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Engine {
    pub name: String,
    #[serde(default)]
    pub host: String,
    /// Actual: Running | Draining | Stopped
    #[serde(default = "default_stopped")]
    pub state: String,
    #[serde(default)]
    pub last_seen: String,
    #[serde(default = "default_ticksec")]
    pub ticksec: u64,
    /// unconditional full reload cadence — the seq fallback
    #[serde(default = "default_full_refresh")]
    pub full_refresh_minutes: u64,
    /// Claude account currently in use (visible to other engines)
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub queuelock: BTreeMap<String, u64>,
    /// per-project machine paths, keyed by project name
    #[serde(default)]
    pub projects: BTreeMap<String, EngineProjectDirs>,
}
fn default_stopped() -> String { "Stopped".into() }
fn default_ticksec() -> u64 { 5 }
fn default_full_refresh() -> u64 { 360 }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkItemTs {
    #[serde(default)]
    pub receive: String,
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub complete: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Tag {
    pub text: String,
    #[serde(default)]
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkItem {
    pub id: String,
    pub project: String,
    /// bumped by iter_data on every write; writers pass expect_version
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_queued")]
    pub state: String,
    /// agent name, or "exec" for shell items
    #[serde(default)]
    pub agent: String,
    /// for agent == "exec": the shell command to run
    #[serde(default)]
    pub exec_shell: String,
    /// lower = sooner; P0 most urgent; default 5
    #[serde(default = "default_priority")]
    pub priority: i64,
    #[serde(default)]
    pub lockdirs: Vec<String>,
    #[serde(default)]
    pub createdby: String,
    #[serde(default)]
    pub requestedby: String,
    #[serde(default)]
    pub blockedby: Vec<String>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub prework: Vec<String>,
    #[serde(default)]
    pub postwork: Vec<String>,
    #[serde(default)]
    pub ts: WorkItemTs,
    #[serde(default)]
    pub tags: Vec<Tag>,
    /// schedule spec — present only on "scheduled" templates (see sched module)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sched: Option<sched::Sched>,
    /// provenance: the template id this run was cloned from
    #[serde(default)]
    pub source_schedule: String,
    /// engine currently running it (set on pick)
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub lasterror: String,
    /// approval-workitem support: signed workid, verified by iter_data
    #[serde(default)]
    pub approval_code: String,
    #[serde(default)]
    pub needs_approval: bool,
}
fn default_queued() -> String { "queued".into() }
fn default_priority() -> i64 { 5 }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkItemDetail {
    pub id: String,
    pub order: i64,
    pub key: String,
    #[serde(default)]
    pub valuetype: String,
    #[serde(default)]
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PrePostWork {
    pub projectname: String,
    pub name: String,
    #[serde(default)]
    pub shell: String,
    #[serde(default = "default_ppw_timeout")]
    pub timeoutsec: u64,
    #[serde(default)]
    pub failhalt: bool,
}
fn default_ppw_timeout() -> u64 { 30 }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebuiUser {
    pub user: String,
    #[serde(default)]
    pub email: String,
    /// user | engine | admin
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default)]
    pub pwhash: String,
    #[serde(default = "default_tokenver")]
    pub tokenver: u64,
    #[serde(default)]
    pub css: String,
    #[serde(default)]
    pub pubkey: String,
    #[serde(default)]
    pub settings: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub authz: BTreeMap<String, String>,
}
fn default_role() -> String { "user".into() }
fn default_tokenver() -> u64 { 1 }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VersionRow {
    pub projectname: String,
    pub table: String,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub updated: String,
}

/// kind: "lock" (held by a running workitem) | "reserve" (scope_reservation barrier)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LockRow {
    pub project: String,
    pub path: String,
    #[serde(default = "default_lock_kind")]
    pub kind: String,
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub workid: String,
    #[serde(default)]
    pub acquired: String,
    #[serde(default)]
    pub expires: String,
}
fn default_lock_kind() -> String { "lock".into() }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectStructure {
    pub projectname: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub snapshot: serde_json::Value,
}

/// Two lock paths overlap when one is an ancestor of (or equal to) the other.
/// Paths are compared after trimming trailing slashes; `{topdir}` prefixes
/// compare literally, which is correct because both sides use the same token.
pub fn paths_overlap(a: &str, b: &str) -> bool {
    let a = a.trim_end_matches('/');
    let b = b.trim_end_matches('/');
    if a == b {
        return true;
    }
    let (shorter, longer) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    longer.starts_with(shorter) && longer.as_bytes().get(shorter.len()) == Some(&b'/')
}

/// Account-ladder pick: exclusion-with-fallback (decided 2026-09-01).
/// `usage` maps account name -> max(5hr%, 7d%); missing = 0 (unknown usage
/// never blocks). `in_use` are accounts other Running engines currently hold.
/// Pass 1 uses `switch` thresholds, pass 2 `stop`; None = all accounts stopped.
pub fn pick_account<'a>(
    accounts: &'a [Account],
    usage: &BTreeMap<String, u8>,
    in_use: &[String],
) -> Option<&'a Account> {
    for threshold in ["switch", "stop"] {
        for exclusion_active in [true, false] {
            let mut sorted: Vec<&Account> = accounts
                .iter()
                .filter(|a| !exclusion_active || !in_use.contains(&a.name))
                .collect();
            sorted.sort_by_key(|a| a.order);
            for acct in sorted {
                let used = usage.get(&acct.name).copied().unwrap_or(0);
                let limit = if threshold == "switch" { acct.switch } else { acct.stop };
                if used < limit || limit == 0 {
                    return Some(acct);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_ancestor() {
        assert!(paths_overlap("{topdir}/src/", "{topdir}/src/deep/child/"));
        assert!(paths_overlap("{topdir}/src/deep/", "{topdir}/src/"));
        assert!(paths_overlap("{topdir}/src", "{topdir}/src/"));
        assert!(!paths_overlap("{topdir}/src/a/", "{topdir}/src/ab/"));
        assert!(!paths_overlap("{topdir}/a/", "{topdir}/b/"));
    }

    fn acct(name: &str, order: i64, switch: u8, stop: u8) -> Account {
        Account { name: name.into(), token_envar: format!("{}_TOKEN", name.to_uppercase()), order, switch, stop }
    }

    #[test]
    fn ladder_prefers_unused_account() {
        let accounts = vec![acct("Dev1", 1, 80, 99), acct("Dev2", 2, 80, 99)];
        let usage = BTreeMap::new();
        let picked = pick_account(&accounts, &usage, &["Dev1".into()]).unwrap();
        assert_eq!(picked.name, "Dev2");
    }

    #[test]
    fn ladder_falls_back_to_shared_when_exclusion_empties() {
        let accounts = vec![acct("Dev1", 1, 80, 99)];
        let usage = BTreeMap::new();
        let picked = pick_account(&accounts, &usage, &["Dev1".into()]).unwrap();
        assert_eq!(picked.name, "Dev1");
    }

    #[test]
    fn ladder_switch_then_stop_then_none() {
        let accounts = vec![acct("Dev1", 1, 80, 99), acct("Dev2", 2, 80, 99)];
        let mut usage = BTreeMap::new();
        usage.insert("Dev1".to_string(), 85u8);
        // Dev1 over switch, Dev2 under: pick Dev2
        assert_eq!(pick_account(&accounts, &usage, &[]).unwrap().name, "Dev2");
        usage.insert("Dev2".to_string(), 90u8);
        // both over switch, both under stop: pass 2 picks Dev1 (order)
        assert_eq!(pick_account(&accounts, &usage, &[]).unwrap().name, "Dev1");
        usage.insert("Dev1".to_string(), 99u8);
        usage.insert("Dev2".to_string(), 99u8);
        // both at stop: nothing
        assert!(pick_account(&accounts, &usage, &[]).is_none());
    }
}
