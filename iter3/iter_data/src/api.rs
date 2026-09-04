//! REST API. Principle (decided 2026-09-01): the state machine lives HERE —
//! webui, engine, and the future MCP layer all call these same rules.
//! Every write bumps the iter3_versions seq for its (project, table).

use rand::seq::SliceRandom;
use crate::auth::{self, Claims};
use crate::storage::{Storage, StorageError, body_str, body_u64};
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::{StatusCode, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine as _;
use iter_core::{LockRow, Project, WebuiUser, WorkItem, now_utc, widget};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// seq bucket for tables that aren't project-scoped (agents, users, engines…)
pub const GLOBAL: &str = "<global>";
/// sort key for single-key tables
const NOSK: &str = "-";

pub struct AppState {
    pub store: Arc<dyn Storage>,
    pub secret: Vec<u8>,
}

type Ctx = State<Arc<AppState>>;

// ---------- error plumbing ----------

pub enum ApiError {
    Status(StatusCode, String),
    Conflict(Value),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::Status(code, msg) => (code, Json(json!({"error": msg}))).into_response(),
            ApiError::Conflict(current) => (
                StatusCode::CONFLICT,
                Json(json!({"error": "conflict", "current": current})),
            )
                .into_response(),
        }
    }
}

impl From<StorageError> for ApiError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::Conflict(cur) => ApiError::Conflict(cur.unwrap_or(Value::Null)),
            StorageError::Backend(msg) => ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR, msg),
        }
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::Status(StatusCode::BAD_REQUEST, msg.into())
}
fn notfound() -> ApiError {
    ApiError::Status(StatusCode::NOT_FOUND, "not found".into())
}
fn forbidden() -> ApiError {
    ApiError::Status(StatusCode::FORBIDDEN, "forbidden".into())
}

// ---------- auth extractor ----------

pub struct AuthUser {
    pub sub: String,
    pub role: String,
}

impl AuthUser {
    fn require_admin(&self) -> Result<(), ApiError> {
        if self.role == "admin" { Ok(()) } else { Err(forbidden()) }
    }
    fn require_writer(&self) -> Result<(), ApiError> {
        // engines and admins and users may all mutate queue-level data; a
        // "viewer" (added 2026-09-04) reads everything and changes nothing but
        // their own profile (timezone, password)
        if ["admin", "engine", "user"].contains(&self.role.as_str()) { Ok(()) } else { Err(forbidden()) }
    }
}

impl FromRequestParts<Arc<AppState>> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let token = header.strip_prefix("Bearer ").unwrap_or("");
        if token.is_empty() {
            return Err(ApiError::Status(StatusCode::UNAUTHORIZED, "missing bearer token".into()));
        }
        let claims: Claims = auth::verify_token(&state.secret, token)
            .map_err(|e| ApiError::Status(StatusCode::UNAUTHORIZED, e))?;
        // tokenver check: the row is the revocation authority
        let row = state
            .store
            .get("webui_user", &claims.sub, NOSK)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::Status(StatusCode::UNAUTHORIZED, "unknown user".into()))?;
        if body_u64(&row, "tokenver") != claims.tokenver {
            return Err(ApiError::Status(StatusCode::UNAUTHORIZED, "token revoked".into()));
        }
        Ok(AuthUser { sub: claims.sub, role: claims.role })
    }
}

// ---------- router ----------

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/auth/login", post(login))
        .route("/api/widget/validate", post(widget_validate))
        // users
        .route("/api/users", get(users_list))
        .route("/api/users/{user}", put(user_put).get(user_get))
        .route("/api/users/{user}/token", post(user_token))
        .route("/api/users/{user}/pubkey", post(user_pubkey))
        // agents
        .route("/api/agents", get(agents_list))
        .route("/api/agents/{name}", get(agent_get).put(agent_put).delete(agent_delete))
        .route("/api/tooling", get(tooling_list))
        .route("/api/tooling/{name}", get(tooling_get).put(tooling_put).delete(tooling_delete))
        // projects
        .route("/api/projects", get(projects_list))
        .route("/api/projects/{name}", get(project_get).put(project_put).delete(project_delete))
        .route("/api/projects/{name}/versions", get(versions_get))
        .route("/api/projects/{name}/status", get(project_status))
        .route("/api/projects/{name}/settle", post(project_settle))
        .route("/api/projects/{name}/spend", get(spend_list).post(spend_add))
        .route("/api/projects/{name}/structure", get(structure_get).put(structure_put))
        .route("/api/projects/{name}/prepostwork", get(prepostwork_list))
        .route("/api/prepostwork/{projectname}/{name}", put(prepostwork_put))
        // engines
        .route("/api/engines", get(engines_list))
        .route("/api/engines/{name}", get(engine_get).put(engine_put).delete(engine_delete))
        .route("/api/engines/{name}/heartbeat", post(engine_heartbeat))
        .route("/api/engines/{name}/test", post(engine_test))
        // workitems
        .route("/api/projects/{name}/workitems", get(workitems_list).post(workitem_create))
        .route(
            "/api/projects/{name}/workitems/{id}",
            get(workitem_get).put(workitem_put).delete(workitem_delete),
        )
        .route("/api/projects/{name}/workitems/{id}/details", get(details_list).post(details_append))
        .route("/api/projects/{name}/workitems/{id}/details/{order}", put(detail_put))
        .route("/api/projects/{name}/workitems/{id}/approve", post(workitem_approve))
        .route("/api/projects/{name}/workitems/{id}/reopen", post(workitem_reopen))
        .route("/api/projects/{name}/workitems/{id}/explain", post(workitem_explain).delete(workitem_explained))
        .route("/api/projects/{name}/workitems/{id}/explain/claim", post(workitem_explain_claim))
        // locks
        .route("/api/projects/{name}/locks", get(locks_list))
        .route("/api/projects/{name}/locks/acquire", post(lock_acquire))
        .route("/api/projects/{name}/locks/release", post(lock_release))
        .route("/api/projects/{name}/locks/extend", post(lock_extend))
        .with_state(state)
}

// ---------- misc ----------

async fn health(State(st): Ctx) -> Json<Value> {
    Json(json!({"ok": true, "backend": st.store.backend_name(), "ts": now_utc()}))
}

async fn widget_validate(_user: AuthUser, Json(body): Json<Value>) -> Json<Value> {
    Json(json!({"errors": widget::validate(&body)}))
}

// ---------- auth ----------

#[derive(serde::Deserialize)]
struct LoginReq {
    user: String,
    password: String,
}

async fn login(State(st): Ctx, Json(req): Json<LoginReq>) -> Result<Json<Value>, ApiError> {
    let row = st
        .store
        .get("webui_user", &req.user, NOSK)
        .await?
        .ok_or_else(|| ApiError::Status(StatusCode::UNAUTHORIZED, "bad credentials".into()))?;
    let pwhash = body_str(&row, "pwhash");
    if pwhash.is_empty() || !auth::verify_password(&req.password, &pwhash) {
        return Err(ApiError::Status(StatusCode::UNAUTHORIZED, "bad credentials".into()));
    }
    let role = body_str(&row, "role");
    let tokenver = body_u64(&row, "tokenver").max(1);
    let token = auth::mint_token(&st.secret, &req.user, &role, tokenver, 24 * 3600)
        .map_err(|e| ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({"token": token, "role": role, "user": req.user, "timezone": body_str(&row, "timezone")})))
}

// ---------- users ----------

fn redact(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.remove("pwhash");
    }
    v
}

async fn users_list(user: AuthUser, State(st): Ctx) -> Result<Json<Value>, ApiError> {
    user.require_admin()?;
    let rows = st.store.scan("webui_user").await?;
    Ok(Json(Value::Array(rows.into_iter().map(redact).collect())))
}

async fn user_get(user: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    if user.role != "admin" && user.sub != name {
        return Err(forbidden());
    }
    let row = st.store.get("webui_user", &name, NOSK).await?.ok_or_else(notfound)?;
    Ok(Json(redact(row)))
}

async fn user_put(
    user: AuthUser,
    State(st): Ctx,
    Path(name): Path<String>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    // admins edit anyone; a user may edit their OWN row, but role / authz /
    // tokenver / pubkey stay whatever they were (self-service = profile + password)
    let self_edit = user.role != "admin";
    if self_edit && user.sub != name {
        return Err(forbidden());
    }
    let existing = st.store.get("webui_user", &name, NOSK).await?;
    if self_edit {
        let Some(ex) = &existing else { return Err(forbidden()) };
        for k in ["role", "authz", "tokenver", "pubkey"] {
            // a key the stored row never had must stay absent (serde defaults
            // apply to missing keys, not to null): a null here failed every
            // non-admin self-edit of a user created without "authz"
            match ex.get(k) {
                Some(v) if !v.is_null() => body[k] = v.clone(),
                _ => {
                    if let Some(o) = body.as_object_mut() {
                        o.remove(k);
                    }
                }
            }
        }
    }
    body["user"] = json!(name);
    // password (plaintext, TLS-transported) -> pwhash; else preserve existing
    if let Some(pw) = body.get("password").and_then(|v| v.as_str()).map(String::from) {
        let hash = auth::hash_password(&pw).map_err(|e| ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        body["pwhash"] = json!(hash);
        body.as_object_mut().unwrap().remove("password");
    } else if body.get("pwhash").map(|v| v.as_str().unwrap_or("").is_empty()).unwrap_or(true) {
        if let Some(ex) = &existing {
            body["pwhash"] = ex.get("pwhash").cloned().unwrap_or(json!(""));
        }
    }
    if body.get("tokenver").and_then(|v| v.as_u64()).unwrap_or(0) == 0 {
        let prior = existing.as_ref().map(|e| body_u64(e, "tokenver")).unwrap_or(0);
        body["tokenver"] = json!(prior.max(1));
    }
    let parsed: WebuiUser =
        serde_json::from_value(body.clone()).map_err(|e| bad(format!("user does not parse: {e}")))?;
    if !["user", "engine", "admin", "viewer"].contains(&parsed.role.as_str()) {
        return Err(bad("role must be user|engine|admin|viewer"));
    }
    st.store.put("webui_user", &name, NOSK, &body).await?;
    st.store.bump_seq(GLOBAL, "webui_user").await?;
    Ok(Json(redact(body)))
}

#[derive(serde::Deserialize)]
struct TokenReq {
    #[serde(default = "default_ttl_days")]
    ttl_days: u64,
}
fn default_ttl_days() -> u64 { 365 }

async fn user_token(
    user: AuthUser,
    State(st): Ctx,
    Path(name): Path<String>,
    Json(req): Json<TokenReq>,
) -> Result<Json<Value>, ApiError> {
    user.require_admin()?;
    let row = st.store.get("webui_user", &name, NOSK).await?.ok_or_else(notfound)?;
    let role = body_str(&row, "role");
    let tokenver = body_u64(&row, "tokenver").max(1);
    let token = auth::mint_token(&st.secret, &name, &role, tokenver, req.ttl_days * 86400)
        .map_err(|e| ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({"token": token, "user": name, "role": role, "ttl_days": req.ttl_days})))
}

#[derive(serde::Deserialize)]
struct PubkeyReq {
    pubkey: String,
    #[serde(default)]
    email: String,
}

/// `iter --adduser` registration path: admin or engine may add/refresh a
/// user's pubkey. Add-only for the key: refuses to overwrite a non-empty one
/// (reset flow is manual via the admin users page, per spec).
async fn user_pubkey(
    user: AuthUser,
    State(st): Ctx,
    Path(name): Path<String>,
    Json(req): Json<PubkeyReq>,
) -> Result<Json<Value>, ApiError> {
    if !["admin", "engine"].contains(&user.role.as_str()) {
        return Err(forbidden());
    }
    let mut row = st.store.get("webui_user", &name, NOSK).await?.unwrap_or_else(|| {
        json!({"user": name, "email": req.email, "role": "user", "pwhash": "", "tokenver": 1,
               "css": "", "pubkey": "", "settings": {}, "authz": {}})
    });
    if !body_str(&row, "pubkey").is_empty() && user.role != "admin" {
        return Err(bad("pubkey already set; reset is manual via an admin (see spec)"));
    }
    row["pubkey"] = json!(req.pubkey);
    st.store.put("webui_user", &name, NOSK, &row).await?;
    st.store.bump_seq(GLOBAL, "webui_user").await?;
    Ok(Json(redact(row)))
}

// ---------- agents ----------

async fn agent_delete(user: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    user.require_admin()?;
    let deleted = st.store.delete("agent", &name, NOSK).await?;
    st.store.bump_seq(GLOBAL, "agent").await?;
    Ok(Json(json!({"deleted": deleted})))
}

// ---------- agent tooling (shared rules, capability docs, source instructions, prose steps, critic) ----------

async fn tooling_list(_u: AuthUser, State(st): Ctx) -> Result<Json<Value>, ApiError> {
    let mut rows = st.store.scan("agent_tooling").await?;
    rows.sort_by(|a, b| body_str(a, "kind").cmp(&body_str(b, "kind")).then(body_str(a, "name").cmp(&body_str(b, "name"))));
    Ok(Json(Value::Array(rows)))
}
async fn tooling_get(_u: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    Ok(Json(st.store.get("agent_tooling", &name, NOSK).await?.ok_or_else(notfound)?))
}
async fn tooling_put(user: AuthUser, State(st): Ctx, Path(name): Path<String>, Json(mut body): Json<Value>) -> Result<Json<Value>, ApiError> {
    user.require_admin()?;
    body["name"] = json!(name);
    let parsed: iter_core::AgentTooling =
        serde_json::from_value(body.clone()).map_err(|e| bad(format!("tooling does not parse: {e}")))?;
    if !iter_core::TOOLING_KINDS.contains(&parsed.kind.as_str()) {
        return Err(bad(format!("kind must be one of {:?}", iter_core::TOOLING_KINDS)));
    }
    st.store.put("agent_tooling", &name, NOSK, &body).await?;
    st.store.bump_seq(GLOBAL, "agent_tooling").await?;
    Ok(Json(body))
}
async fn tooling_delete(user: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    user.require_admin()?;
    let deleted = st.store.delete("agent_tooling", &name, NOSK).await?;
    st.store.bump_seq(GLOBAL, "agent_tooling").await?;
    Ok(Json(json!({"deleted": deleted})))
}

async fn agents_list(_u: AuthUser, State(st): Ctx) -> Result<Json<Value>, ApiError> {
    Ok(Json(Value::Array(st.store.scan("agent").await?)))
}

async fn agent_get(_u: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    Ok(Json(st.store.get("agent", &name, NOSK).await?.ok_or_else(notfound)?))
}

async fn agent_put(
    user: AuthUser,
    State(st): Ctx,
    Path(name): Path<String>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    user.require_admin()?;
    body["name"] = json!(name);
    let _: iter_core::AgentDef =
        serde_json::from_value(body.clone()).map_err(|e| bad(format!("agent does not parse: {e}")))?;
    st.store.put("agent", &name, NOSK, &body).await?;
    st.store.bump_seq(GLOBAL, "agent").await?;
    Ok(Json(body))
}

// ---------- projects ----------

async fn projects_list(_u: AuthUser, State(st): Ctx) -> Result<Json<Value>, ApiError> {
    Ok(Json(Value::Array(st.store.scan("project").await?)))
}

async fn project_get(_u: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    Ok(Json(st.store.get("project", &name, NOSK).await?.ok_or_else(notfound)?))
}

async fn project_put(
    user: AuthUser,
    State(st): Ctx,
    Path(name): Path<String>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    user.require_admin()?;
    body["name"] = json!(name);
    let parsed: Project =
        serde_json::from_value(body.clone()).map_err(|e| bad(format!("project does not parse: {e}")))?;
    if !["Running", "Draining", "Stopped"].contains(&parsed.state.as_str()) {
        return Err(bad("project state must be Running|Draining|Stopped"));
    }
    st.store.put("project", &name, NOSK, &body).await?;
    st.store.bump_seq(&name, "project").await?;
    Ok(Json(body))
}

async fn project_delete(user: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    user.require_admin()?;
    let deleted = st.store.delete("project", &name, NOSK).await?;
    st.store.bump_seq(&name, "project").await?;
    Ok(Json(json!({"deleted": deleted})))
}

async fn versions_get(_u: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    let mut rows = st.store.get_versions(&name).await?;
    rows.extend(st.store.get_versions(GLOBAL).await?);
    Ok(Json(serde_json::to_value(rows).unwrap_or(Value::Null)))
}

/// Draining monitoring (spec: "carefully monitor all engines for possible
/// disconnections, to make sure all engines are honoring the command").
/// Computes, centrally: per-engine liveness (last_seen vs 3x ticksec + 5s
/// grace), in-progress counts, whether the drain has completed, and which
/// engines might NOT be honoring it (disconnected while holding work).
async fn project_status(_u: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    let project = st.store.get("project", &name, NOSK).await?.ok_or_else(notfound)?;
    let project_state = body_str(&project, "state");
    let items = st.store.query("workitem", &name).await?;
    let mut inprogress_by_engine: HashMap<String, i64> = HashMap::new();
    for i in &items {
        if body_str(i, "state") == "in-progress" {
            *inprogress_by_engine.entry(body_str(i, "engine")).or_insert(0) += 1;
        }
    }
    let now = chrono::Utc::now();
    let mut engines_out = Vec::new();
    let mut not_honoring = Vec::new();
    for e in st.store.scan("engine").await? {
        // only engines that serve this project
        if e.get("projects").and_then(|p| p.get(&name)).is_none() {
            continue;
        }
        let ename = body_str(&e, "name");
        let ticksec = e.get("ticksec").and_then(|t| t.as_u64()).unwrap_or(5);
        let last_seen = body_str(&e, "last_seen");
        let age_sec = chrono::DateTime::parse_from_rfc3339(&last_seen)
            .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds())
            .unwrap_or(i64::MAX);
        let stale = age_sec > (3 * ticksec as i64 + 5);
        let running = inprogress_by_engine.get(&ename).copied().unwrap_or(0);
        if project_state == "Draining" && stale && running > 0 {
            not_honoring.push(ename.clone());
        }
        engines_out.push(json!({
            "name": ename,
            "state": body_str(&e, "state"),
            "last_seen": last_seen,
            "age_sec": if age_sec == i64::MAX { Value::Null } else { json!(age_sec) },
            "stale": stale,
            "inprogress": running,
        }));
    }
    let total_inprogress: i64 = inprogress_by_engine.values().sum();
    Ok(Json(json!({
        "project": name,
        "project_state": project_state,
        "engines": engines_out,
        "inprogress": total_inprogress,
        "all_drained": total_inprogress == 0,
        "not_honoring": not_honoring,
    })))
}

/// Draining is transitional (decided 2026-09-04): once nothing is in progress
/// on any live engine, the project settles to Stopped. Engines call this each
/// tick while Draining; the webui calls it on refresh so a drain with nothing
/// running settles at once even with no engine up. Writer role suffices —
/// it can only ever move Draining -> Stopped, and only when drained.
async fn project_settle(user: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    let mut proj = st.store.get("project", &name, NOSK).await?.ok_or_else(notfound)?;
    if body_str(&proj, "state") != "Draining" {
        return Ok(Json(json!({"state": body_str(&proj, "state"), "settled": false})));
    }
    let items = st.store.query("workitem", &name).await?;
    let inprogress = items.iter().filter(|i| body_str(i, "state") == "in-progress").count();
    if inprogress > 0 {
        return Ok(Json(json!({"state": "Draining", "settled": false, "inprogress": inprogress})));
    }
    proj["state"] = json!("Stopped");
    st.store.put("project", &name, NOSK, &proj).await?;
    st.store.bump_seq(&name, "project").await?;
    Ok(Json(json!({"state": "Stopped", "settled": true})))
}

// ---------- spend (per project per UTC day; engines add after each run) ----------

async fn spend_list(_u: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    let mut rows = st.store.query("spend", &name).await?;
    rows.sort_by(|a, b| body_str(b, "date").cmp(&body_str(a, "date")));
    rows.truncate(31);
    Ok(Json(Value::Array(rows)))
}

#[derive(serde::Deserialize)]
struct SpendReq {
    #[serde(default)]
    usd: f64,
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    workid: String,
}

/// Add one run's cost to today's row (read-modify-write; a lost race under-
/// counts by one run, which the daily cap tolerates).
async fn spend_add(user: AuthUser, State(st): Ctx, Path(name): Path<String>, Json(req): Json<SpendReq>) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    let date = now_utc()[..10].to_string();
    let mut row = st.store.get("spend", &name, &date).await?.unwrap_or(json!({
        "project": name, "date": date, "usd": 0.0, "input_tokens": 0, "output_tokens": 0, "runs": 0
    }));
    row["usd"] = json!(row.get("usd").and_then(|v| v.as_f64()).unwrap_or(0.0) + req.usd);
    row["input_tokens"] = json!(body_u64(&row, "input_tokens") + req.input_tokens);
    row["output_tokens"] = json!(body_u64(&row, "output_tokens") + req.output_tokens);
    row["runs"] = json!(body_u64(&row, "runs") + 1);
    row["last_workid"] = json!(req.workid);
    row["updated"] = json!(now_utc());
    st.store.put("spend", &name, &date, &row).await?;
    Ok(Json(row))
}

// ---------- structure ----------

async fn structure_get(_u: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    Ok(Json(st.store.get("project_structure", &name, NOSK).await?.unwrap_or(Value::Null)))
}

async fn structure_put(
    user: AuthUser,
    State(st): Ctx,
    Path(name): Path<String>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    body["projectname"] = json!(name);
    body["updated"] = json!(now_utc());
    st.store.put("project_structure", &name, NOSK, &body).await?;
    st.store.bump_seq(&name, "project_structure").await?;
    Ok(Json(body))
}

// ---------- prepostwork ----------

async fn prepostwork_list(_u: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    // merged view: project-specific rows win over <default> by "name"
    let mut by_name: HashMap<String, Value> = HashMap::new();
    for row in st.store.query("project_prepostwork", "<default>").await? {
        by_name.insert(body_str(&row, "name"), row);
    }
    for row in st.store.query("project_prepostwork", &name).await? {
        by_name.insert(body_str(&row, "name"), row);
    }
    let mut rows: Vec<Value> = by_name.into_values().collect();
    rows.sort_by_key(|r| body_str(r, "name"));
    Ok(Json(Value::Array(rows)))
}

async fn prepostwork_put(
    user: AuthUser,
    State(st): Ctx,
    Path((projectname, name)): Path<(String, String)>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    user.require_admin()?;
    body["projectname"] = json!(projectname);
    body["name"] = json!(name);
    let _: iter_core::PrePostWork =
        serde_json::from_value(body.clone()).map_err(|e| bad(format!("prepostwork does not parse: {e}")))?;
    st.store.put("project_prepostwork", &projectname, &name, &body).await?;
    st.store.bump_seq(&projectname, "project_prepostwork").await?;
    Ok(Json(body))
}

// ---------- engines ----------

async fn engines_list(_u: AuthUser, State(st): Ctx) -> Result<Json<Value>, ApiError> {
    Ok(Json(Value::Array(st.store.scan("engine").await?)))
}

async fn engine_get(_u: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    Ok(Json(st.store.get("engine", &name, NOSK).await?.ok_or_else(notfound)?))
}

async fn engine_put(
    user: AuthUser,
    State(st): Ctx,
    Path(name): Path<String>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    body["name"] = json!(name);
    let _: iter_core::Engine =
        serde_json::from_value(body.clone()).map_err(|e| bad(format!("engine does not parse: {e}")))?;
    st.store.put("engine", &name, NOSK, &body).await?;
    st.store.bump_seq(GLOBAL, "engine").await?;
    Ok(Json(body))
}

#[derive(serde::Deserialize)]
struct HeartbeatReq {
    #[serde(default)]
    state: String,
    #[serde(default)]
    account: String,
    /// latest usage snapshot for the active account (engine-owned)
    #[serde(default)]
    usage: Option<Value>,
    /// outcome of a connectivity test the engine just ran
    #[serde(default)]
    test_result: Option<Value>,
    /// the engine consumed test_requested
    #[serde(default)]
    clear_test: bool,
}

/// webui -> engine: ask for a connectivity nudge (`claude -p "."` on haiku);
/// the engine sees `test_requested` on its next tick and answers via heartbeat.
async fn engine_test(user: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    let mut row = st.store.get("engine", &name, NOSK).await?.ok_or_else(notfound)?;
    let ts = now_utc();
    row["test_requested"] = json!(ts);
    st.store.put("engine", &name, NOSK, &row).await?;
    st.store.bump_seq(GLOBAL, "engine").await?;
    Ok(Json(json!({"requested": ts})))
}

/// Remove an engine record (admin). A record that heartbeated within three
/// ticks is alive and refused: stop the engine first. Any project listing the
/// engine drops it from its `engines` list.
async fn engine_delete(user: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    user.require_admin()?;
    let row = st.store.get("engine", &name, NOSK).await?.ok_or_else(notfound)?;
    let tick = row.get("ticksec").and_then(|t| t.as_i64()).unwrap_or(5).max(1);
    let alive = chrono::DateTime::parse_from_rfc3339(&body_str(&row, "last_seen"))
        .map(|seen| (chrono::Utc::now() - seen.with_timezone(&chrono::Utc)).num_seconds() <= 3 * tick + 5)
        .unwrap_or(false);
    if alive {
        return Err(ApiError::Status(StatusCode::CONFLICT, format!("engine '{name}' is heartbeating — stop it before deleting its record")));
    }
    let deleted = st.store.delete("engine", &name, NOSK).await?;
    st.store.bump_seq(GLOBAL, "engine").await?;
    for mut p in st.store.scan("project").await? {
        let list: Vec<Value> = p.get("engines").and_then(|e| e.as_array()).cloned().unwrap_or_default();
        if list.iter().any(|e| e.as_str() == Some(name.as_str())) {
            let pname = body_str(&p, "name");
            p["engines"] = Value::Array(list.into_iter().filter(|e| e.as_str() != Some(name.as_str())).collect());
            st.store.put("project", &pname, NOSK, &p).await?;
            st.store.bump_seq(&pname, "project").await?;
        }
    }
    Ok(Json(json!({"deleted": deleted})))
}

async fn engine_heartbeat(
    user: AuthUser,
    State(st): Ctx,
    Path(name): Path<String>,
    Json(req): Json<HeartbeatReq>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    let mut row = st.store.get("engine", &name, NOSK).await?.ok_or_else(notfound)?;
    row["last_seen"] = json!(now_utc());
    if !req.state.is_empty() {
        row["state"] = json!(req.state);
    }
    if !req.account.is_empty() {
        row["account"] = json!(req.account);
    }
    if let Some(u) = req.usage {
        row["usage"] = u;
    }
    if let Some(t) = req.test_result {
        row["test_result"] = t;
    }
    if req.clear_test {
        row["test_requested"] = json!("");
    }
    st.store.put("engine", &name, NOSK, &row).await?;
    st.store.bump_seq(GLOBAL, "engine").await?;
    Ok(Json(row))
}

// ---------- workitems ----------

async fn workitems_list(
    _u: AuthUser,
    State(st): Ctx,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let mut rows = st.store.query("workitem", &name).await?;
    if let Some(state) = q.get("state") {
        rows.retain(|r| body_str(r, "state") == *state);
    }
    Ok(Json(Value::Array(rows)))
}

fn normalize_new_item(project: &str, mut body: Value) -> Result<(String, Value), ApiError> {
    body["project"] = json!(project);
    if body_str(&body, "id").is_empty() {
        body["id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    body["version"] = json!(1);
    if body_str(&body, "state").is_empty() {
        body["state"] = json!("queued"); // queued is the default create-state (spec)
    }
    if body.get("ts").map(|t| body_str(t, "receive").is_empty()).unwrap_or(true) {
        body["ts"] = json!({"receive": now_utc(), "start": "", "complete": ""});
    }
    let parsed: WorkItem =
        serde_json::from_value(body.clone()).map_err(|e| bad(format!("workitem does not parse: {e}")))?;
    if !iter_core::STATES.contains(&parsed.state.as_str()) {
        return Err(bad(format!("unknown state '{}'", parsed.state)));
    }
    Ok((parsed.id, body))
}

async fn workitem_create(
    user: AuthUser,
    State(st): Ctx,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    // users-only rule (itersched.md): schedules come from humans via the
    // webui/API — the engine role (the agents' path) may not create them
    if user.role == "engine"
        && (body_str(&body, "state") == "scheduled" || body.get("sched").map(|s| !s.is_null()).unwrap_or(false))
    {
        return Err(ApiError::Status(
            StatusCode::FORBIDDEN,
            "schedules are users-only: the engine/agent path may not create scheduled items".into(),
        ));
    }
    let (id, body) = normalize_new_item(&name, body)?;
    st.store.put_versioned("workitem", &name, &id, &body, 0).await?;
    st.store.bump_seq(&name, "workitem").await?;
    Ok(Json(body))
}

async fn workitem_get(
    _u: AuthUser,
    State(st): Ctx,
    Path((name, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(st.store.get("workitem", &name, &id).await?.ok_or_else(notfound)?))
}

async fn workitem_put(
    user: AuthUser,
    State(st): Ctx,
    Path((name, id)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    let expect: u64 = q
        .get("expect_version")
        .and_then(|v| v.parse().ok())
        .or_else(|| body.get("expect_version").and_then(|v| v.as_u64()))
        .ok_or_else(|| bad("expect_version required (query param or body field)"))?;
    if let Some(o) = body.as_object_mut() {
        o.remove("expect_version");
    }
    // closed items are immutable (decided 2026-09-03): append a "doc" detail
    // row, or POST .../reopen — never edit the record in place.  The one
    // exception is "tags", so finished work can still be organized.
    if let Some(current) = st.store.get("workitem", &name, &id).await? {
        if is_closed(&body_str(&current, "state")) && !tags_only_change(&current, &body) {
            return Err(closed_err());
        }
    }
    body["project"] = json!(name);
    body["id"] = json!(id);
    body["version"] = json!(expect + 1);
    let parsed: WorkItem =
        serde_json::from_value(body.clone()).map_err(|e| bad(format!("workitem does not parse: {e}")))?;
    if !iter_core::STATES.contains(&parsed.state.as_str()) {
        return Err(bad(format!("unknown state '{}'", parsed.state)));
    }
    st.store.put_versioned("workitem", &name, &id, &body, expect).await?;
    st.store.bump_seq(&name, "workitem").await?;
    Ok(Json(body))
}

async fn workitem_delete(
    user: AuthUser,
    State(st): Ctx,
    Path((name, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    let deleted = st.store.delete("workitem", &name, &id).await?;
    st.store.bump_seq(&name, "workitem").await?;
    Ok(Json(json!({"deleted": deleted})))
}

// ---------- workitem details ----------

async fn details_list(
    _u: AuthUser,
    State(st): Ctx,
    Path((_name, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let mut rows = st.store.query("workitem_detail", &id).await?;
    // sk is a string; sort numerically by order
    rows.sort_by_key(|r| r.get("order").and_then(|v| v.as_i64()).unwrap_or(0));
    Ok(Json(Value::Array(rows)))
}

/// Closed states (decided 2026-09-03): the record is frozen; only "doc"
/// detail rows may be appended, and only `reopen` moves it back to queued.
pub const CLOSED_STATES: &[&str] = &["complete", "failed"];
/// The one detail key that may be appended to a closed item.
pub const DOC_KEY: &str = "doc";
/// the ELI5 row the engine appends (spec: Explain / ELI5); like "doc", it may
/// land on a closed item
pub const EXPLAINED_KEY: &str = "explained";

pub fn is_closed(state: &str) -> bool {
    CLOSED_STATES.contains(&state)
}

/// True when `proposed` differs from `current` in nothing but "tags"
/// (version is the write's own bump and is ignored).
pub fn tags_only_change(current: &Value, proposed: &Value) -> bool {
    let strip = |v: &Value| -> Value {
        let mut c = v.clone();
        if let Some(o) = c.as_object_mut() {
            o.remove("tags");
            o.remove("version");
            o.remove("expect_version");
        }
        c
    };
    strip(current) == strip(proposed)
}

fn closed_err() -> ApiError {
    ApiError::Status(
        StatusCode::FORBIDDEN,
        "closed workitem is immutable: append a \"doc\" detail row (POST .../details) or POST .../reopen".into(),
    )
}

/// Validate + provenance-stamp a detail body.  Every detail write records
/// who (JWT principal) and when, so a closeout note is a real record.
fn prepare_detail(user: &AuthUser, id: &str, order: i64, mut body: Value) -> Result<Value, ApiError> {
    body["id"] = json!(id);
    body["order"] = json!(order);
    body["by"] = json!(user.sub);
    body["ts"] = json!(now_utc());
    let parsed: iter_core::WorkItemDetail =
        serde_json::from_value(body.clone()).map_err(|e| bad(format!("detail does not parse: {e}")))?;
    if parsed.key.trim().is_empty() {
        return Err(bad("detail key is required"));
    }
    // question widgets are validated at write time so malformed ones bounce here
    if parsed.valuetype == "json" && parsed.value.get("fields").is_some() {
        let errs = widget::validate(&parsed.value);
        if !errs.is_empty() {
            return Err(bad(format!("widget invalid: {}", errs.join("; "))));
        }
    }
    Ok(body)
}

/// zero-pad sk so lexical order == numeric order in backends
fn detail_sk(order: i64) -> String {
    format!("{order:010}")
}

async fn detail_put(
    user: AuthUser,
    State(st): Ctx,
    Path((name, id, order)): Path<(String, String, i64)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    // in-place detail writes (question answers overwrite value) are for OPEN items only
    if let Some(item) = st.store.get("workitem", &name, &id).await? {
        if is_closed(&body_str(&item, "state")) {
            return Err(closed_err());
        }
    }
    let body = prepare_detail(&user, &id, order, body)?;
    st.store.put("workitem_detail", &id, &detail_sk(order), &body).await?;
    st.store.bump_seq(&name, "workitem_detail").await?;
    Ok(Json(body))
}

/// Append a detail row: iter_data allocates the next order atomically
/// (create-if-absent on the zero-padded sk, retried on a lost race), so two
/// appenders can never overwrite each other.  On a closed item only "doc"
/// rows are accepted.
async fn details_append(
    user: AuthUser,
    State(st): Ctx,
    Path((name, id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    let item = st.store.get("workitem", &name, &id).await?.ok_or_else(notfound)?;
    // closed items take only appended notes: "doc" (humans), "explained" (the
    // ELI5 run) and the "spend" row that run costs — never a new response
    let key = body_str(&body, "key");
    if is_closed(&body_str(&item, "state")) && key != DOC_KEY && key != EXPLAINED_KEY && key != "spend" {
        return Err(ApiError::Status(
            StatusCode::FORBIDDEN,
            format!("closed workitem: only \"{DOC_KEY}\", \"{EXPLAINED_KEY}\" and \"spend\" detail rows may be appended"),
        ));
    }
    append_detail(&st, &user, &name, &id, body).await
}

async fn append_detail(st: &Arc<AppState>, user: &AuthUser, name: &str, id: &str, body: Value) -> Result<Json<Value>, ApiError> {
    for _ in 0..8 {
        let next = st
            .store
            .query("workitem_detail", id)
            .await?
            .iter()
            .map(|r| r.get("order").and_then(|o| o.as_i64()).unwrap_or(0))
            .max()
            .map(|m| m + 1)
            .unwrap_or(0);
        let row = prepare_detail(user, id, next, body.clone())?;
        match st.store.put_versioned("workitem_detail", id, &detail_sk(next), &row, 0).await {
            Ok(()) => {
                st.store.bump_seq(name, "workitem_detail").await?;
                return Ok(Json(row));
            }
            Err(StorageError::Conflict(_)) => continue, // lost the race for this order; re-read
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::Status(StatusCode::CONFLICT, "could not allocate a detail order (contention)".into()))
}

// ---------- explain (ELI5) ----------

/// webui -> engine: ask for a plain-language explanation of this item (spec:
/// Explain / ELI5). Stamps `explain_requested`; the engine serving the project
/// sees it on its next tick and runs the read-only `explain` agent at once,
/// outside the agent cap. Works on closed items too (no state change).
async fn workitem_explain(
    user: AuthUser,
    State(st): Ctx,
    Path((name, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    let mut item = st.store.get("workitem", &name, &id).await?.ok_or_else(notfound)?;
    let pending = body_str(&item, "explain_requested");
    if !pending.is_empty() {
        return Ok(Json(json!({"requested": pending, "already": true})));
    }
    let ts = now_utc();
    let expect = body_u64(&item, "version");
    // one engine only (decided 2026-09-04): pick at random among the LIVE
    // engines serving this project so a second engine never duplicates the
    // run; none live = leave it open for the first engine to claim
    let engine = live_engines_for(&st, &name).await?.choose(&mut rand::thread_rng()).cloned().unwrap_or_default();
    item["explain_requested"] = json!(ts);
    item["explain_engine"] = json!(engine);
    item["version"] = json!(expect + 1);
    st.store.put_versioned("workitem", &name, &id, &item, expect).await?;
    st.store.bump_seq(&name, "workitem").await?;
    Ok(Json(json!({"requested": ts, "engine": engine})))
}

/// Engines whose record names this project and that have heartbeated within
/// three ticks (the webui's own liveness rule).
async fn live_engines_for(st: &Arc<AppState>, project: &str) -> Result<Vec<String>, ApiError> {
    let now = chrono::Utc::now();
    Ok(st
        .store
        .scan("engine")
        .await?
        .iter()
        .filter(|e| e.get("projects").and_then(|p| p.get(project)).is_some())
        .filter(|e| body_str(e, "state") == "Running")
        .filter(|e| {
            let tick = e.get("ticksec").and_then(|t| t.as_i64()).unwrap_or(5).max(1);
            chrono::DateTime::parse_from_rfc3339(&body_str(e, "last_seen"))
                .map(|seen| (now - seen.with_timezone(&chrono::Utc)).num_seconds() <= 3 * tick + 5)
                .unwrap_or(false)
        })
        .map(|e| body_str(e, "name"))
        .filter(|n| !n.is_empty())
        .collect())
}

#[derive(serde::Deserialize, Default)]
struct ExplainClaimReq {
    #[serde(default)]
    engine: String,
}

/// engine -> iter_data: "I will run this ELI5". Succeeds when the item is
/// assigned to this engine or to nobody (then it becomes this engine's);
/// 409 when another engine holds it, so at most one engine ever runs it.
async fn workitem_explain_claim(
    user: AuthUser,
    State(st): Ctx,
    Path((name, id)): Path<(String, String)>,
    Json(req): Json<ExplainClaimReq>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    if req.engine.trim().is_empty() {
        return Err(bad("engine is required"));
    }
    let mut item = st.store.get("workitem", &name, &id).await?.ok_or_else(notfound)?;
    if body_str(&item, "explain_requested").is_empty() {
        return Err(bad("no ELI5 is pending on this workitem"));
    }
    let holder = body_str(&item, "explain_engine");
    if !holder.is_empty() && holder != req.engine {
        return Err(ApiError::Status(StatusCode::CONFLICT, format!("ELI5 on this workitem is assigned to engine '{holder}'")));
    }
    if holder.is_empty() {
        let expect = body_u64(&item, "version");
        item["explain_engine"] = json!(req.engine);
        item["version"] = json!(expect + 1);
        st.store.put_versioned("workitem", &name, &id, &item, expect).await?;
        st.store.bump_seq(&name, "workitem").await?;
    }
    Ok(Json(json!({"engine": req.engine})))
}

/// engine -> iter_data: the explanation landed (or could not be produced);
/// clear the flag so the button re-arms.
async fn workitem_explained(
    user: AuthUser,
    State(st): Ctx,
    Path((name, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    let mut item = st.store.get("workitem", &name, &id).await?.ok_or_else(notfound)?;
    if body_str(&item, "explain_requested").is_empty() {
        return Ok(Json(json!({"cleared": false})));
    }
    let expect = body_u64(&item, "version");
    item["explain_requested"] = json!("");
    item["explain_engine"] = json!("");
    item["version"] = json!(expect + 1);
    st.store.put_versioned("workitem", &name, &id, &item, expect).await?;
    st.store.bump_seq(&name, "workitem").await?;
    Ok(Json(json!({"cleared": true})))
}

// ---------- reopen ----------

#[derive(serde::Deserialize, Default)]
struct ReopenReq {
    #[serde(default)]
    reason: String,
}

/// Reopen a closed item (users-only, like schedules): back to queued with the
/// bounce counter reset, and a "doc" row recording who reopened it and why.
/// Consequence (spec): downstream items still queued become blocked again.
async fn workitem_reopen(
    user: AuthUser,
    State(st): Ctx,
    Path((name, id)): Path<(String, String)>,
    body: Option<Json<ReopenReq>>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    if user.role == "engine" {
        return Err(ApiError::Status(StatusCode::FORBIDDEN, "reopen is users-only: the engine/agent path may not reopen closed items".into()));
    }
    let req = body.map(|b| b.0).unwrap_or_default();
    let mut item = st.store.get("workitem", &name, &id).await?.ok_or_else(notfound)?;
    if !is_closed(&body_str(&item, "state")) {
        return Err(bad("workitem is not closed"));
    }
    let expect = body_u64(&item, "version");
    let was = body_str(&item, "state");
    item["state"] = json!("queued");
    item["gate_bounces"] = json!(0);
    item["lasterror"] = json!("");
    item["ts"]["complete"] = json!("");
    item["version"] = json!(expect + 1);
    st.store.put_versioned("workitem", &name, &id, &item, expect).await?;
    st.store.bump_seq(&name, "workitem").await?;
    let note = if req.reason.trim().is_empty() {
        format!("reopened by {} (was {was})", user.sub)
    } else {
        format!("reopened by {} (was {was}): {}", user.sub, req.reason.trim())
    };
    let _ = append_detail(&st, &user, &name, &id, json!({"key": DOC_KEY, "valuetype": "text", "value": note})).await?;
    Ok(Json(item))
}

// ---------- approval ----------

#[derive(serde::Deserialize)]
struct ApproveReq {
    user: String,
    /// base64 of the 64-byte ed25519 signature over the workitem id (utf-8)
    signature: String,
}

async fn workitem_approve(
    caller: AuthUser,
    State(st): Ctx,
    Path((name, id)): Path<(String, String)>,
    Json(req): Json<ApproveReq>,
) -> Result<Json<Value>, ApiError> {
    caller.require_writer()?;
    let mut item = st.store.get("workitem", &name, &id).await?.ok_or_else(notfound)?;
    let user_row = st
        .store
        .get("webui_user", &req.user, NOSK)
        .await?
        .ok_or_else(|| bad("unknown approving user"))?;
    let pubkey_b64 = body_str(&user_row, "pubkey");
    let verified = verify_ed25519(&pubkey_b64, &id, &req.signature);
    let expect = body_u64(&item, "version");
    if verified {
        item["approval_code"] = json!(req.signature);
        item["needs_approval"] = json!(false);
        item["state"] = json!("queued");
        item["version"] = json!(expect + 1);
        st.store.put_versioned("workitem", &name, &id, &item, expect).await?;
        st.store.bump_seq(&name, "workitem").await?;
        Ok(Json(json!({"approved": true, "item": item})))
    } else {
        // spec: clear the approval code, log the failure, leave it needing approval
        item["approval_code"] = json!("");
        item["version"] = json!(expect + 1);
        st.store.put_versioned("workitem", &name, &id, &item, expect).await?;
        st.store.bump_seq(&name, "workitem").await?;
        eprintln!("[iter_data] approval FAILED for workitem {id} by {}", req.user);
        Err(bad("signature did not verify against the user's pubkey"))
    }
}

fn verify_ed25519(pubkey_b64: &str, message: &str, sig_b64: &str) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let Ok(pk_bytes) = base64::engine::general_purpose::STANDARD.decode(pubkey_b64.trim()) else {
        return false;
    };
    let Ok(pk_arr): Result<[u8; 32], _> = pk_bytes.as_slice().try_into() else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&pk_arr) else {
        return false;
    };
    let Ok(sig_bytes) = base64::engine::general_purpose::STANDARD.decode(sig_b64.trim()) else {
        return false;
    };
    let Ok(sig_arr): Result<[u8; 64], _> = sig_bytes.as_slice().try_into() else {
        return false;
    };
    vk.verify(message.as_bytes(), &Signature::from_bytes(&sig_arr)).is_ok()
}

// ---------- locks ----------

async fn locks_list(_u: AuthUser, State(st): Ctx, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    Ok(Json(Value::Array(st.store.query("lock", &name).await?)))
}

#[derive(serde::Deserialize)]
struct LockAcquireReq {
    path: String,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default)]
    engine: String,
    workid: String,
    #[serde(default = "default_ttl")]
    ttl_sec: i64,
}
fn default_kind() -> String { "lock".into() }
fn default_ttl() -> i64 { 3600 }

async fn lock_acquire(
    user: AuthUser,
    State(st): Ctx,
    Path(name): Path<String>,
    Json(req): Json<LockAcquireReq>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    let now = now_utc();
    let expires = (chrono::Utc::now() + chrono::Duration::seconds(req.ttl_sec))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let row = LockRow {
        project: name.clone(),
        path: req.path.clone(),
        kind: req.kind,
        engine: req.engine,
        workid: req.workid.clone(),
        acquired: now.clone(),
        expires,
    };
    let body = serde_json::to_value(&row).unwrap();
    st.store.acquire_lock("lock", &name, &req.path, &body, &now, &req.workid).await?;
    st.store.bump_seq(&name, "lock").await?;
    Ok(Json(body))
}

#[derive(serde::Deserialize)]
struct LockReleaseReq {
    path: String,
    workid: String,
    #[serde(default)]
    ttl_sec: i64,
}

async fn lock_release(
    user: AuthUser,
    State(st): Ctx,
    Path(name): Path<String>,
    Json(req): Json<LockReleaseReq>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    if let Some(row) = st.store.get("lock", &name, &req.path).await? {
        if body_str(&row, "workid") != req.workid {
            return Err(bad("lock held by a different workid"));
        }
        st.store.delete("lock", &name, &req.path).await?;
        st.store.bump_seq(&name, "lock").await?;
    }
    Ok(Json(json!({"released": true})))
}

async fn lock_extend(
    user: AuthUser,
    State(st): Ctx,
    Path(name): Path<String>,
    Json(req): Json<LockReleaseReq>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    let mut row = st.store.get("lock", &name, &req.path).await?.ok_or_else(notfound)?;
    if body_str(&row, "workid") != req.workid {
        return Err(bad("lock held by a different workid"));
    }
    let ttl = if req.ttl_sec > 0 { req.ttl_sec } else { 3600 };
    row["expires"] = json!(
        (chrono::Utc::now() + chrono::Duration::seconds(ttl))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    );
    let now = now_utc();
    st.store.acquire_lock("lock", &name, &req.path, &row, &now, &req.workid).await?;
    st.store.bump_seq(&name, "lock").await?;
    Ok(Json(row))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_items_accept_only_tag_changes() {
        let cur = json!({"id":"a","state":"complete","priority":5,"tags":[],"version":3});
        let tagged = json!({"id":"a","state":"complete","priority":5,"tags":[{"text":"regressed","color":"#f00"}],"version":3});
        assert!(tags_only_change(&cur, &tagged));
        let reprioritized = json!({"id":"a","state":"complete","priority":1,"tags":[{"text":"x","color":""}],"version":3});
        assert!(!tags_only_change(&cur, &reprioritized));
        let reopened = json!({"id":"a","state":"queued","priority":5,"tags":[],"version":3});
        assert!(!tags_only_change(&cur, &reopened));
        assert!(is_closed("complete") && is_closed("failed") && !is_closed("question"));
    }
}
