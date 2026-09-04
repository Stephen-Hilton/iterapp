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
    /// completion contract the engine enforces at close (spec: Close Gate)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closegate: Option<CloseGate>,
}

/// Per-agent close gate (decided 2026-09-03): what must be true before an
/// item may close complete.  Every key is overridable per project via
/// `project.agents[agent].closegate`; see `close_gate_for`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CloseGate {
    /// verifier model alias (haiku | sonnet | opus | ...); "" disables the LLM half
    #[serde(default = "default_verify")]
    pub verify: String,
    /// the item must have created >=1 workitem with createdby == its id
    #[serde(default)]
    pub requires_children: bool,
    /// the enforced git postwork must have produced a new commit
    #[serde(default)]
    pub requires_commit: bool,
    /// bounces back to queued before the item goes to question
    #[serde(default = "default_max_bounces")]
    pub max_bounces: u32,
    /// turn cap for the verifier session
    #[serde(default = "default_verify_max_turns")]
    pub verify_max_turns: u32,
}
fn default_verify() -> String { "haiku".into() }
fn default_max_bounces() -> u32 { 1 }
fn default_verify_max_turns() -> u32 { 8 }
impl Default for CloseGate {
    fn default() -> Self {
        Self {
            verify: default_verify(),
            requires_children: false,
            requires_commit: false,
            max_bounces: default_max_bounces(),
            verify_max_turns: default_verify_max_turns(),
        }
    }
}

/// Resolve the effective close gate: agent-def defaults, then the project's
/// per-agent override merged key-by-key (an override may set just one key).
pub fn close_gate_for(agent_def: &serde_json::Value, project_override: &serde_json::Value) -> CloseGate {
    let mut merged = agent_def.get("closegate").cloned().unwrap_or(serde_json::json!({}));
    if !merged.is_object() {
        merged = serde_json::json!({});
    }
    if let Some(ovr) = project_override.get("closegate").and_then(|v| v.as_object()) {
        for (k, v) in ovr {
            merged[k] = v.clone();
        }
    }
    serde_json::from_value(merged).unwrap_or_default()
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
    /// engine-owned: latest usage snapshot for the active account
    /// {account, five_hour_pct, seven_day_pct, ..., ts} (heartbeat)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
    /// webui-owned: ISO ts of a pending connectivity test ("" = none)
    #[serde(default)]
    pub test_requested: String,
    /// engine-owned: outcome of the last connectivity test
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_result: Option<serde_json::Value>,
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
    /// opt out of DEEP dependencies (workitem_dependency.md): by default a
    /// blocker is satisfied only when it AND everything it created (createdby,
    /// transitively) closed complete; shallow = the blocker alone
    #[serde(default)]
    pub blockedby_shallow: bool,
    #[serde(default)]
    pub attempt: u32,
    /// close-gate bounces so far (spec: Close Gate); reset by a human requeue
    #[serde(default)]
    pub gate_bounces: u32,
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
    /// operator override (spec: Run Now): start on the next tick once deps
    /// and locks allow, even when the maxagents cap is full; cleared on claim
    #[serde(default)]
    pub run_now: bool,
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
    /// provenance, stamped by iter_data on every write: JWT principal + UTC ts
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub ts: String,
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

/// Dependency gate (workitem_dependency.md, V2 semantics kept in V3).
#[derive(Debug, Clone, PartialEq)]
pub enum DepStatus {
    Satisfied,
    /// still waiting on this item (the blocker itself, or one of its descendants)
    Waiting(String),
    /// this blocker (or a descendant) closed failed: never release, park for review
    Failed(String),
}

/// creator id -> items it created (createdby)
pub fn children_index(items: &[WorkItem]) -> std::collections::HashMap<String, Vec<&WorkItem>> {
    let mut idx: std::collections::HashMap<String, Vec<&WorkItem>> = std::collections::HashMap::new();
    for i in items {
        if !i.createdby.is_empty() {
            idx.entry(i.createdby.clone()).or_default().push(i);
        }
    }
    idx
}

/// Deep by default: every blocker must be complete and so must every item it
/// created, transitively. Unknown ids (deleted) count as satisfied. Cycle-safe.
pub fn dependency_status(
    item: &WorkItem,
    by_id: &std::collections::HashMap<String, &WorkItem>,
    children: &std::collections::HashMap<String, Vec<&WorkItem>>,
) -> DepStatus {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for dep in &item.blockedby {
        let Some(d) = by_id.get(dep) else { continue };
        if d.state == "failed" {
            return DepStatus::Failed(d.id.clone());
        }
        if d.state != "complete" {
            return DepStatus::Waiting(d.id.clone());
        }
        if item.blockedby_shallow {
            continue;
        }
        let mut stack: Vec<&WorkItem> = children.get(dep).map(|v| v.clone()).unwrap_or_default();
        while let Some(c) = stack.pop() {
            if c.id == item.id || !seen.insert(c.id.clone()) {
                continue;
            }
            if c.state == "failed" {
                return DepStatus::Failed(c.id.clone());
            }
            if c.state != "complete" {
                return DepStatus::Waiting(c.id.clone());
            }
            if let Some(more) = children.get(&c.id) {
                stack.extend(more.iter().copied());
            }
        }
    }
    DepStatus::Satisfied
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

    fn wi(id: &str, state: &str, createdby: &str, blockedby: &[&str]) -> WorkItem {
        WorkItem { id: id.into(), state: state.into(), createdby: createdby.into(),
            blockedby: blockedby.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn deep_dependencies_wait_for_descendants_and_park_on_failure() {
        let items = vec![
            wi("plan", "complete", "user", &[]),
            wi("child", "queued", "plan", &[]),
            wi("grandchild", "complete", "child", &[]),
            wi("dep", "queued", "user", &["plan"]),
        ];
        let by_id: std::collections::HashMap<String, &WorkItem> = items.iter().map(|i| (i.id.clone(), i)).collect();
        let kids = children_index(&items);
        assert_eq!(dependency_status(&items[3], &by_id, &kids), DepStatus::Waiting("child".into()));
        let mut shallow = items[3].clone();
        shallow.blockedby_shallow = true;
        assert_eq!(dependency_status(&shallow, &by_id, &kids), DepStatus::Satisfied);
        // descendant failed -> Failed; unknown blocker -> satisfied
        let mut failed = items.clone();
        failed[1].state = "failed".into();
        let by2: std::collections::HashMap<String, &WorkItem> = failed.iter().map(|i| (i.id.clone(), i)).collect();
        let kids2 = children_index(&failed);
        assert_eq!(dependency_status(&failed[3], &by2, &kids2), DepStatus::Failed("child".into()));
        let ghost = wi("g", "queued", "", &["nope"]);
        assert_eq!(dependency_status(&ghost, &by_id, &kids), DepStatus::Satisfied);
    }

    #[test]
    fn close_gate_merges_override_keywise() {
        let def = serde_json::json!({"closegate": {"verify": "sonnet", "requires_children": true}});
        let ovr = serde_json::json!({"closegate": {"max_bounces": 2}});
        let g = close_gate_for(&def, &ovr);
        assert_eq!(g.verify, "sonnet");
        assert!(g.requires_children);
        assert_eq!(g.max_bounces, 2);
        // absent everywhere -> defaults (haiku, one bounce)
        let g = close_gate_for(&serde_json::json!({}), &serde_json::Value::Null);
        assert_eq!(g, CloseGate::default());
        assert_eq!(g.verify, "haiku");
        // override can disable the verifier
        let g = close_gate_for(&def, &serde_json::json!({"closegate": {"verify": ""}}));
        assert_eq!(g.verify, "");
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
