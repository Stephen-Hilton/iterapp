//! iter_data — the central API server + persistence for iter V3.
//! Runs local (sqlite, zero-config) or against DynamoDB (remote). Handlers
//! are transport-agnostic axum; cargo-lambda can wrap the same router later.

mod api;
mod auth;
mod ddb;
mod sqlite;
mod storage;

use api::{AppState, GLOBAL};
use clap::Parser;
use serde_json::json;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "iter_data", about = "iter V3 central data/API server")]
struct Args {
    /// storage backend: sqlite | dynamodb
    #[arg(long, default_value = "sqlite")]
    backend: String,
    /// sqlite database file
    #[arg(long, default_value = "./iter3.db")]
    db: String,
    /// dynamodb region (falls back to AWS_DEFAULT_REGION)
    #[arg(long, default_value = "")]
    region: String,
    /// dynamodb table prefix — tables are created additively under this prefix only
    #[arg(long, default_value = "iter3_")]
    prefix: String,
    /// listen address
    #[arg(long, default_value = "127.0.0.1:8300")]
    listen: String,
    /// static webui directory ('' disables)
    #[arg(long, default_value = "")]
    webui_dir: String,
    /// JWT secret sidecar file (ITER_JWT_SECRET env wins)
    #[arg(long, default_value = "./iter_data.secret")]
    secret_file: String,
    /// .env file to load (KEY=VALUE lines; malformed lines skipped)
    #[arg(long, default_value = "./.env")]
    env_file: String,
}

/// Minimal .env loader: KEY=VALUE lines only, no shell semantics — the user's
/// .env contains lines a shell would choke on, so never `source` it.
fn load_env_file(path: &str) {
    let Ok(content) = std::fs::read_to_string(path) else { return };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if !k.is_empty() && !k.contains(char::is_whitespace) && std::env::var(k).is_err() {
                unsafe { std::env::set_var(k, v) };
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    load_env_file(&args.env_file);

    let store: Arc<dyn storage::Storage> = match args.backend.as_str() {
        "sqlite" => Arc::new(sqlite::SqliteBackend::open(&args.db).expect("open sqlite")),
        "dynamodb" => {
            let region = if !args.region.is_empty() {
                args.region.clone()
            } else {
                std::env::var("AWS_DEFAULT_REGION").unwrap_or_else(|_| "us-west-2".into())
            };
            if !args.prefix.starts_with("iter3") {
                // guardrail: never operate outside our own namespace
                eprintln!("refusing prefix '{}' — must start with 'iter3'", args.prefix);
                std::process::exit(2);
            }
            Arc::new(ddb::DdbBackend::new(&region, &args.prefix).await.expect("dynamodb init"))
        }
        other => {
            eprintln!("unknown backend '{other}' (expected sqlite | dynamodb)");
            std::process::exit(2);
        }
    };

    bootstrap_admin(store.as_ref()).await;

    let secret = auth::load_secret(&args.secret_file);
    let state = Arc::new(AppState { store, secret });

    let mut app = api::router(state).layer(
        tower_http::cors::CorsLayer::new()
            .allow_origin(tower_http::cors::Any)
            .allow_methods(tower_http::cors::Any)
            .allow_headers(tower_http::cors::Any),
    );
    if !args.webui_dir.is_empty() {
        app = app.fallback_service(tower_http::services::ServeDir::new(&args.webui_dir));
    }

    let listener = tokio::net::TcpListener::bind(&args.listen).await.expect("bind");
    println!("[iter_data] listening on {} (backend: {})", args.listen, args.backend);
    axum::serve(listener, app).await.expect("serve");
}

/// First-run bootstrap: if no users exist, create "admin".
/// Password from ITER_ADMIN_PASSWORD, else generated and printed ONCE.
async fn bootstrap_admin(store: &dyn storage::Storage) {
    let users = store.scan("webui_user").await.unwrap_or_default();
    if !users.is_empty() {
        return;
    }
    let (password, generated) = match std::env::var("ITER_ADMIN_PASSWORD") {
        Ok(p) if !p.trim().is_empty() => (p.trim().to_string(), false),
        _ => {
            use rand::Rng;
            let p: String = rand::thread_rng()
                .sample_iter(&rand::distributions::Alphanumeric)
                .take(20)
                .map(char::from)
                .collect();
            (p, true)
        }
    };
    let pwhash = auth::hash_password(&password).expect("hash admin password");
    let row = json!({
        "user": "admin", "email": "", "role": "admin", "pwhash": pwhash,
        "tokenver": 1, "css": "", "pubkey": "", "settings": {}, "authz": {}
    });
    store.put("webui_user", "admin", "-", &row).await.expect("bootstrap admin");
    let _ = store.bump_seq(GLOBAL, "webui_user").await;
    if generated {
        println!("[iter_data] bootstrapped user 'admin' with password: {password}");
        println!("[iter_data] (set ITER_ADMIN_PASSWORD to control this; change via the users API)");
    } else {
        println!("[iter_data] bootstrapped user 'admin' (password from ITER_ADMIN_PASSWORD)");
    }
}
