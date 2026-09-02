//! REST API. Principle (decided 2026-09-01): the state machine lives HERE —
//! webui, engine, and the future MCP layer all call these same rules.
//! Every write bumps the iter3_versions seq for its (project, table).

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
        // engines and admins and users may all mutate queue-level data
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
        .route("/api/agents/{name}", get(agent_get).put(agent_put))
        // projects
        .route("/api/projects", get(projects_list))
        .route("/api/projects/{name}", get(project_get).put(project_put).delete(project_delete))
        .route("/api/projects/{name}/versions", get(versions_get))
        .route("/api/projects/{name}/structure", get(structure_get).put(structure_put))
        .route("/api/projects/{name}/prepostwork", get(prepostwork_list))
        .route("/api/prepostwork/{projectname}/{name}", put(prepostwork_put))
        // engines
        .route("/api/engines", get(engines_list))
        .route("/api/engines/{name}", get(engine_get).put(engine_put))
        .route("/api/engines/{name}/heartbeat", post(engine_heartbeat))
        // workitems
        .route("/api/projects/{name}/workitems", get(workitems_list).post(workitem_create))
        .route(
            "/api/projects/{name}/workitems/{id}",
            get(workitem_get).put(workitem_put).delete(workitem_delete),
        )
        .route("/api/projects/{name}/workitems/{id}/details", get(details_list))
        .route("/api/projects/{name}/workitems/{id}/details/{order}", put(detail_put))
        .route("/api/projects/{name}/workitems/{id}/approve", post(workitem_approve))
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
    Ok(Json(json!({"token": token, "role": role, "user": req.user})))
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
    user.require_admin()?;
    let existing = st.store.get("webui_user", &name, NOSK).await?;
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
    if !["user", "engine", "admin"].contains(&parsed.role.as_str()) {
        return Err(bad("role must be user|engine|admin"));
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

async fn detail_put(
    user: AuthUser,
    State(st): Ctx,
    Path((name, id, order)): Path<(String, String, i64)>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    user.require_writer()?;
    body["id"] = json!(id);
    body["order"] = json!(order);
    let parsed: iter_core::WorkItemDetail =
        serde_json::from_value(body.clone()).map_err(|e| bad(format!("detail does not parse: {e}")))?;
    // question widgets are validated at write time so malformed ones bounce here
    if parsed.valuetype == "json" && parsed.value.get("fields").is_some() {
        let errs = widget::validate(&parsed.value);
        if !errs.is_empty() {
            return Err(bad(format!("widget invalid: {}", errs.join("; "))));
        }
    }
    // zero-pad sk so lexical order == numeric order in backends
    let sk = format!("{order:010}");
    st.store.put("workitem_detail", &id, &sk, &body).await?;
    st.store.bump_seq(&name, "workitem_detail").await?;
    Ok(Json(body))
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
