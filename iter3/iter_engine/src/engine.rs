//! The tick loop: sync metadata (seq-gated with a periodic full-refresh
//! fallback), heartbeat, pick queued work, take central locks, run, close.

use crate::client::Api;
use iter_core::{Engine, Project, WorkItem, now_utc, paths_overlap, pick_account};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub struct EngineRuntime {
    api: Api,
    name: String,
    /// last seq seen per (project, table)
    seen_seq: HashMap<(String, String), u64>,
    last_full_refresh: Instant,
    /// cached data
    projects: HashMap<String, Project>,
    items: HashMap<String, Vec<WorkItem>>,
    agents: HashMap<String, Value>,
    /// retry backoff: workid -> eligible-at
    deferred: HashMap<String, Instant>,
    /// running work: (workid, agent_type, handle)
    running: Vec<(String, String, std::thread::JoinHandle<()>)>,
    running_count: Arc<AtomicUsize>,
    pub max_ticks: Option<u64>,
}

/// max(5hr%,7d%) usage per account. TODO: port the V2 tracking (`acct 5h 30%
/// · 7d 14%`) — until then unknown usage reads as 0, which never blocks.
fn usage_map() -> BTreeMap<String, u8> {
    BTreeMap::new()
}

/// maxagents ladder: among ">N%" gates where usage > N pick the LARGEST N
/// (most restrictive true gate — order-independent equivalent of the spec's
/// top-down list); fall through to "else" (default 4 when absent).
fn max_agents(gates: &BTreeMap<String, u32>, usage_pct: u8) -> u32 {
    let mut best: Option<(u8, u32)> = None;
    for (k, v) in gates {
        if let Some(n) = k.strip_prefix('>').and_then(|s| s.strip_suffix('%')).and_then(|s| s.parse::<u8>().ok()) {
            if usage_pct > n && best.map(|(bn, _)| n > bn).unwrap_or(true) {
                best = Some((n, *v));
            }
        }
    }
    if let Some((_, v)) = best {
        return v;
    }
    gates.get("else").copied().unwrap_or(4)
}

fn expand_topdir(topdir: &str) -> String {
    if let Some(rest) = topdir.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    topdir.to_string()
}

impl EngineRuntime {
    pub fn new(api: Api, name: String) -> Self {
        Self {
            api,
            name,
            seen_seq: HashMap::new(),
            last_full_refresh: Instant::now() - Duration::from_secs(86400 * 365),
            projects: HashMap::new(),
            items: HashMap::new(),
            agents: HashMap::new(),
            deferred: HashMap::new(),
            running: Vec::new(),
            running_count: Arc::new(AtomicUsize::new(0)),
            max_ticks: None,
        }
    }

    pub fn run(&mut self) {
        let mut ticks: u64 = 0;
        loop {
            ticks += 1;
            let engine = match self.api.get(&format!("/api/engines/{}", self.name)) {
                Ok(v) => match serde_json::from_value::<Engine>(v) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("[engine] bad engine record: {e}");
                        std::thread::sleep(Duration::from_secs(5));
                        continue;
                    }
                },
                Err(e) => {
                    eprintln!(
                        "[engine] cannot load engine '{}' from iter_data ({e}); define it via the API/webui first",
                        self.name
                    );
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
            };

            self.tick(&engine);

            if let Some(max) = self.max_ticks {
                if ticks >= max {
                    println!("[engine] max ticks reached, draining running work");
                    while self.prune_running() > 0 {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    return;
                }
            }
            std::thread::sleep(Duration::from_secs(engine.ticksec.max(1)));
        }
    }

    fn prune_running(&mut self) -> usize {
        self.running.retain(|(_, _, h)| !h.is_finished());
        self.running.len()
    }

    fn tick(&mut self, engine: &Engine) {
        self.prune_running();

        // account selection: exclusion-with-fallback against other Running engines
        let in_use: Vec<String> = self
            .api
            .get("/api/engines")
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter(|e| {
                e.get("name").and_then(|n| n.as_str()) != Some(self.name.as_str())
                    && e.get("state").and_then(|s| s.as_str()) == Some("Running")
            })
            .filter_map(|e| e.get("account").and_then(|a| a.as_str()).map(String::from))
            .filter(|a| !a.is_empty())
            .collect();

        let usage = usage_map();
        let mut chosen_account = String::new();
        for project_name in engine.projects.keys() {
            if let Some(p) = self.projects.get(project_name) {
                if let Some(acct) = pick_account(&p.accounts, &usage, &in_use) {
                    chosen_account = acct.name.clone();
                    break;
                }
            }
        }

        // heartbeat: actual state + account, every tick
        let _ = self.api.post(
            &format!("/api/engines/{}/heartbeat", self.name),
            &json!({"state": "Running", "account": chosen_account}),
        );

        // metadata + queue sync, seq-gated with the periodic full-refresh fallback
        let full_refresh = self.last_full_refresh.elapsed()
            > Duration::from_secs(engine.full_refresh_minutes.max(1) * 60);
        if full_refresh {
            self.last_full_refresh = Instant::now();
        }

        for project_name in engine.projects.keys() {
            let versions = self
                .api
                .get(&format!("/api/projects/{project_name}/versions"))
                .ok()
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();
            let seq_of = |table: &str| -> u64 {
                versions
                    .iter()
                    .find(|r| r.get("table").and_then(|t| t.as_str()) == Some(table))
                    .and_then(|r| r.get("seq").and_then(|s| s.as_u64()))
                    .unwrap_or(0)
            };

            let mut reload = |rt: &mut Self, table: &str| -> bool {
                let key = (project_name.clone(), table.to_string());
                let now_seq = seq_of(table);
                let changed = rt.seen_seq.get(&key).copied() != Some(now_seq);
                if changed || full_refresh {
                    rt.seen_seq.insert(key, now_seq);
                    true
                } else {
                    false
                }
            };

            if reload(self, "project") {
                if let Ok(v) = self.api.get(&format!("/api/projects/{project_name}")) {
                    if let Ok(p) = serde_json::from_value::<Project>(v) {
                        self.projects.insert(project_name.clone(), p);
                    }
                }
            }
            if reload(self, "agent") {
                if let Ok(v) = self.api.get("/api/agents") {
                    for a in v.as_array().cloned().unwrap_or_default() {
                        if let Some(n) = a.get("name").and_then(|n| n.as_str()) {
                            self.agents.insert(n.to_string(), a.clone());
                        }
                    }
                }
            }
            if reload(self, "workitem") {
                if let Ok(v) = self.api.get(&format!("/api/projects/{project_name}/workitems")) {
                    let items: Vec<WorkItem> = v
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|i| serde_json::from_value(i).ok())
                        .collect();
                    self.items.insert(project_name.clone(), items);
                }
            }

            let Some(project) = self.projects.get(project_name).cloned() else { continue };
            if project.state != "Running" {
                continue; // Draining/Stopped: finish running work, start nothing new
            }
            let Some(dirs) = engine.projects.get(project_name) else { continue };
            let topdir = expand_topdir(dirs.dirs.get("topdir").map(String::as_str).unwrap_or("."));
            self.dispatch(engine, &project, &topdir);
        }
    }

    fn dispatch(&mut self, engine: &Engine, project: &Project, topdir: &str) {
        let project_name = project.name.clone();
        let items = self.items.get(&project_name).cloned().unwrap_or_default();
        let by_id: HashMap<String, &WorkItem> = items.iter().map(|i| (i.id.clone(), i)).collect();

        let usage_pct = 0u8; // see usage_map TODO
        let cap = max_agents(&project.maxagents, usage_pct) as usize;
        let running_now = self.running.len();
        if running_now >= cap {
            return;
        }

        // current central lock rows (locks + reservations)
        let lock_rows: Vec<Value> = self
            .api
            .get(&format!("/api/projects/{project_name}/locks"))
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let locked_paths: Vec<(String, String, String)> = lock_rows
            .iter()
            .map(|r| {
                (
                    r.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string(),
                    r.get("kind").and_then(|k| k.as_str()).unwrap_or("lock").to_string(),
                    r.get("workid").and_then(|w| w.as_str()).unwrap_or("").to_string(),
                )
            })
            .collect();

        let deps_satisfied = |item: &WorkItem| -> bool {
            item.blockedby.iter().all(|dep| {
                by_id.get(dep).map(|d| d.state == "complete").unwrap_or(true)
            })
        };
        let scope_blocked = |item: &WorkItem| -> bool {
            item.lockdirs.iter().any(|d| {
                locked_paths.iter().any(|(path, kind, workid)| {
                    workid != &item.id && kind == "lock" && paths_overlap(d, path)
                })
            })
        };

        let mut queued: Vec<&WorkItem> = items
            .iter()
            .filter(|i| i.state == "queued" && !i.needs_approval)
            .filter(|i| {
                self.deferred
                    .get(&i.id)
                    .map(|until| Instant::now() >= *until)
                    .unwrap_or(true)
            })
            .filter(|i| deps_satisfied(i))
            .collect();
        queued.sort_by_key(|i| (i.priority, i.ts.receive.clone()));

        // scope reservation (central "reserve" rows): the best dispatchable-but-
        // scope-blocked item reserves its paths so new overlapping work stops
        // being admitted; strictly-better priority still barges.
        let reserver: Option<&WorkItem> = queued.iter().copied().find(|i| scope_blocked(i));
        if let Some(r) = reserver {
            for d in &r.lockdirs {
                let _ = self.api.post(
                    &format!("/api/projects/{project_name}/locks/acquire"),
                    &json!({"path": d, "kind": "reserve", "engine": self.name,
                            "workid": r.id, "ttl_sec": 600}),
                );
            }
        }
        let reserved: Vec<(String, i64, String)> = locked_paths
            .iter()
            .filter(|(_, kind, _)| kind == "reserve")
            .filter_map(|(path, _, workid)| {
                by_id.get(workid).map(|i| (path.clone(), i.priority, workid.clone()))
            })
            .collect();

        let mut slots = cap.saturating_sub(running_now);
        for item in queued {
            if slots == 0 {
                break;
            }
            if scope_blocked(item) {
                continue;
            }
            // reservation gate: overlapping a reserved scope requires strictly
            // better (lower) priority than the reserver; the reserver is exempt
            let gated = item.lockdirs.iter().any(|d| {
                reserved.iter().any(|(path, rprio, rworkid)| {
                    rworkid != &item.id && paths_overlap(d, path) && item.priority >= *rprio
                })
            });
            if gated {
                continue;
            }
            // per-agent-type cap (project override "max", else agent default)
            let type_running =
                self.running.iter().filter(|(_, a, _)| a == &item.agent).count();
            let type_max = project
                .agents
                .get(&item.agent)
                .and_then(|o| o.get("max"))
                .and_then(|m| m.as_u64())
                .or_else(|| {
                    self.agents.get(&item.agent).and_then(|a| a.get("max")).and_then(|m| m.as_u64())
                })
                .unwrap_or(4) as usize;
            if item.agent != "exec" && type_running >= type_max {
                continue;
            }
            if self.start_item(engine, project, topdir, item) {
                slots -= 1;
            }
        }
    }

    fn start_item(
        &mut self,
        _engine: &Engine,
        project: &Project,
        topdir: &str,
        item: &WorkItem,
    ) -> bool {
        // claim: queued -> in-progress via versioned write (loses race gracefully)
        let mut claimed = serde_json::to_value(item).unwrap();
        claimed["state"] = json!("in-progress");
        claimed["engine"] = json!(self.name);
        claimed["attempt"] = json!(item.attempt + 1);
        claimed["ts"]["start"] = json!(now_utc());
        let resp = self.api.put(
            &format!(
                "/api/projects/{}/workitems/{}?expect_version={}",
                project.name, item.id, item.version
            ),
            &claimed,
        );
        let claimed_item: WorkItem = match resp.and_then(|v| {
            serde_json::from_value(v).map_err(|e| crate::client::ApiError { status: 0, body: e.to_string() })
        }) {
            Ok(i) => i,
            Err(e) => {
                if e.status != 409 {
                    eprintln!("[engine] claim failed for {}: {e}", item.id);
                }
                return false;
            }
        };

        // central locks for every lockdir
        let ttl = 3600 + 300;
        let mut acquired: Vec<String> = Vec::new();
        for d in &item.lockdirs {
            let res = self.api.post(
                &format!("/api/projects/{}/locks/acquire", project.name),
                &json!({"path": d, "kind": "lock", "engine": self.name,
                        "workid": item.id, "ttl_sec": ttl}),
            );
            if res.is_err() {
                for p in &acquired {
                    let _ = self.api.post(
                        &format!("/api/projects/{}/locks/release", project.name),
                        &json!({"path": p, "workid": item.id}),
                    );
                }
                // lost the lock race: put it back to queued
                let mut back = serde_json::to_value(&claimed_item).unwrap();
                back["state"] = json!("queued");
                let _ = self.api.put(
                    &format!(
                        "/api/projects/{}/workitems/{}?expect_version={}",
                        project.name, item.id, claimed_item.version
                    ),
                    &back,
                );
                self.deferred.insert(item.id.clone(), Instant::now() + Duration::from_secs(5));
                return false;
            }
            acquired.push(d.clone());
        }

        println!(
            "[engine] start {} '{}' (agent {}, P{})",
            &item.id[..8.min(item.id.len())],
            item.name,
            item.agent,
            item.priority
        );
        let api = self.api.clone();
        let engine_name = self.name.clone();
        let project = project.clone();
        let topdir = topdir.to_string();
        let run_item = claimed_item;
        let counter = self.running_count.clone();
        counter.fetch_add(1, Ordering::SeqCst);
        let agent_type = run_item.agent.clone();
        let workid = run_item.id.clone();
        let handle = std::thread::spawn(move || {
            crate::work::execute(&api, &engine_name, &project, &topdir, run_item);
            counter.fetch_sub(1, Ordering::SeqCst);
        });
        self.running.push((workid, agent_type, handle));
        true
    }
}
