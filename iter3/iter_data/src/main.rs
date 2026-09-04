//! iter_data — the central API server + persistence for iter V3.
//! Runs local (sqlite, zero-config) or against DynamoDB (remote). Handlers
//! are transport-agnostic axum; cargo-lambda can wrap the same router later.

mod api;
mod auth;
mod ddb;
mod migrate;
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

    /// one-shot V2 import: path to a V2 .iter/.engine/iter.db (then exit)
    #[arg(long, default_value = "")]
    migrate_v2: String,
    /// V3 project name for --migrate-v2
    #[arg(long, default_value = "")]
    migrate_project: String,
    /// absolute V2 topdir (rewritten to "{topdir}" in lockdirs)
    #[arg(long, default_value = "")]
    migrate_topdir: String,
    /// engine-side topdir written into the engine record (default: the same path)
    #[arg(long, default_value = "")]
    migrate_engine_topdir: String,
    /// engine record name for --migrate-v2
    #[arg(long, default_value = "Engine01")]
    migrate_engine: String,
    /// V2 .iter/agents directory (agent defs; _shared.md appended to each)
    #[arg(long, default_value = "")]
    migrate_agents_dir: String,
    /// V2 main.iter.md (project name/description)
    #[arg(long, default_value = "")]
    migrate_mainfile: String,
    /// replace rows that already exist (default: skip them)
    #[arg(long, default_value_t = false)]
    migrate_overwrite: bool,
    /// count and report only; write nothing
    #[arg(long, default_value_t = false)]
    migrate_dry_run: bool,
}

/// The single-file webui, embedded so the Lambda deploy needs no filesystem.
const WEBUI_INDEX: &str = include_str!("../../webui/index.html");

async fn embedded_index(uri: axum::http::Uri) -> axum::response::Response {
    use axum::response::IntoResponse;
    match uri.path() {
        "/" | "/index.html" => axum::response::Html(WEBUI_INDEX).into_response(),
        _ => (axum::http::StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

fn in_lambda() -> bool {
    std::env::var("AWS_LAMBDA_RUNTIME_API").is_ok()
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
    let mut args = Args::parse();
    load_env_file(&args.env_file);
    // Lambda: the runtime starts the bootstrap with no arguments, so the
    // backend is DynamoDB by definition and the prefix comes from ITER_PREFIX
    if in_lambda() {
        args.backend = "dynamodb".into();
        if let Ok(p) = std::env::var("ITER_PREFIX") {
            if !p.trim().is_empty() {
                args.prefix = p.trim().to_string();
            }
        }
    }

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

    if !args.migrate_v2.is_empty() {
        return migrate_v2(store.as_ref(), &args).await;
    }

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
    } else {
        app = app.fallback(embedded_index);
    }

    if in_lambda() {
        // function-URL / API Gateway events -> the same axum router
        if let Err(e) = lambda_http::run(app).await {
            eprintln!("[iter_data] lambda runtime error: {e}");
        }
        return;
    }
    let listener = tokio::net::TcpListener::bind(&args.listen).await.expect("bind");
    println!("[iter_data] listening on {} (backend: {})", args.listen, args.backend);
    axum::serve(listener, app).await.expect("serve");
}

/// `--migrate-v2`: import a V2 project into this backend, print a report, exit.
async fn migrate_v2(store: &dyn storage::Storage, args: &Args) {
    if args.migrate_project.trim().is_empty() || args.migrate_topdir.trim().is_empty() {
        eprintln!("--migrate-v2 needs --migrate-project <name> and --migrate-topdir </abs/path>");
        std::process::exit(2);
    }
    let opts = migrate::Options {
        db_path: args.migrate_v2.clone(),
        project: args.migrate_project.trim().to_string(),
        topdir_abs: args.migrate_topdir.trim().to_string(),
        engine_topdir: if args.migrate_engine_topdir.trim().is_empty() {
            args.migrate_topdir.trim().to_string()
        } else {
            args.migrate_engine_topdir.trim().to_string()
        },
        engine_name: args.migrate_engine.clone(),
        agents_dir: args.migrate_agents_dir.clone(),
        mainfile: args.migrate_mainfile.clone(),
        overwrite: args.migrate_overwrite,
        dry_run: args.migrate_dry_run,
    };
    match migrate::run(store, &opts).await {
        Ok(rep) => {
            println!(
                "[migrate-v2] {}project '{}' <- {}",
                if opts.dry_run { "DRY RUN: " } else { "" },
                opts.project,
                opts.db_path
            );
            println!("  workitems written: {} (skipped existing: {})", rep.items_written, rep.items_skipped);
            for (st, n) in &rep.items_by_state {
                println!("    {st}: {n}");
            }
            println!("  detail rows: {} (review rows from critiques: {})", rep.details_written, rep.reviews);
            println!("  agents written: {} (skipped existing: {})", rep.agents_written, rep.agents_skipped);
            println!("  project record written: {}; engine record written: {}", rep.project_written, rep.engine_written);
            match &rep.user_written {
                Some(u) => println!("  operator user upserted from ITER_USERNAME/ITER_PASSWORD: {u}"),
                None => println!("  operator user: ITER_USERNAME/ITER_PASSWORD not set, none written"),
            }
            for w in &rep.warnings {
                println!("  warning: {w}");
            }
        }
        Err(e) => {
            eprintln!("[migrate-v2] FAILED: {e}");
            std::process::exit(1);
        }
    }
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
