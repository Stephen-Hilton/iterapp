//! iter_engine — the local engine binary for iter V3.
//! Holds only its connection config (.iter/config.json) + .env credentials;
//! everything else lives in iter_data.

mod client;
mod engine;
mod usage;
mod work;

use base64::Engine as _;
use clap::Parser;
use client::Api;
use serde_json::json;

#[derive(Parser, Debug)]
#[command(name = "iter_engine", about = "iter V3 local engine")]
struct Args {
    /// engine config file (data_url, token_envar, engine_name, env_file)
    #[arg(long, default_value = ".iter/config.json")]
    config: String,
    /// run N ticks then drain and exit (0 = forever); used by tests
    #[arg(long, default_value_t = 0)]
    ticks: u64,
    /// create a user keypair + register the pubkey, then exit
    #[arg(long)]
    adduser: Option<String>,
    /// sign an approval for a workitem id (or unique id prefix), then exit
    #[arg(long)]
    approve: Option<String>,
    /// private key path for --approve (else ITER_APPROVE_KEYPATH env)
    #[arg(long)]
    pvtkeypath: Option<String>,
    /// approving user name for --approve (default: key file stem)
    #[arg(long)]
    user: Option<String>,
    /// print configured account envars and whether each is set, then exit
    #[arg(long, default_value_t = false)]
    accounts: bool,
    /// validate a question-widget json file, then exit
    #[arg(long)]
    question_widget: Option<String>,
}

#[derive(serde::Deserialize)]
struct EngineConfig {
    data_url: String,
    #[serde(default = "default_token_envar")]
    token_envar: String,
    engine_name: String,
    #[serde(default = "default_env_file")]
    env_file: String,
}
fn default_token_envar() -> String { "ITER_ENGINE_TOKEN".into() }
fn default_env_file() -> String { "./.env".into() }

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

fn main() {
    let args = Args::parse();

    let cfg: EngineConfig = match std::fs::read_to_string(&args.config)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
    {
        Ok(c) => c,
        Err(e) => {
            // helpers that don't need iter_data may still work without config
            if let Some(path) = &args.question_widget {
                return question_widget(path);
            }
            eprintln!("cannot read engine config {}: {e}", args.config);
            eprintln!("expected: {{\"data_url\": \"http://127.0.0.1:8300\", \"token_envar\": \"ITER_ENGINE_TOKEN\", \"engine_name\": \"Engine01\", \"env_file\": \"./.env\"}}");
            std::process::exit(2);
        }
    };
    load_env_file(&cfg.env_file);
    let token = std::env::var(&cfg.token_envar).unwrap_or_default();
    let api = Api::new(&cfg.data_url, &token);

    if let Some(path) = &args.question_widget {
        return question_widget(path);
    }
    if let Some(name) = &args.adduser {
        return adduser(&api, name);
    }
    if let Some(workid) = &args.approve {
        return approve(&api, workid, args.pvtkeypath.as_deref(), args.user.as_deref());
    }
    if args.accounts {
        return accounts(&api);
    }

    if token.is_empty() {
        eprintln!("no engine token: set {} (mint via POST /api/users/<engine-user>/token as admin)", cfg.token_envar);
        std::process::exit(2);
    }
    usage::install_collector();
    let mut rt = engine::EngineRuntime::new(api, cfg.engine_name.clone());
    if args.ticks > 0 {
        rt.max_ticks = Some(args.ticks);
    }
    println!("[engine] {} starting against {}", cfg.engine_name, cfg.data_url);
    rt.run();
}

fn question_widget(path: &str) {
    let content = if path == "-" {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).ok();
        s
    } else {
        std::fs::read_to_string(path).unwrap_or_default()
    };
    match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(v) => {
            let errs = iter_core::widget::validate(&v);
            if errs.is_empty() {
                println!("OK: widget is valid");
            } else {
                for e in &errs {
                    println!("INVALID: {e}");
                }
                std::process::exit(1);
            }
        }
        Err(e) => {
            println!("INVALID: not json: {e}");
            std::process::exit(1);
        }
    }
}

/// `iter --adduser "stephen"` (decided 2026-09-01): keypair to
/// .iter/users/<name>.pem (add-only), gitignore the users dir, register the
/// pubkey with iter_data when a connection is available.
fn adduser(api: &Api, name: &str) {
    use ed25519_dalek::SigningKey;
    use ed25519_dalek::pkcs8::EncodePrivateKey;

    let users_dir = std::path::Path::new(".iter/users");
    std::fs::create_dir_all(users_dir).expect("create .iter/users");
    let keypath = users_dir.join(format!("{name}.pem"));
    if keypath.exists() {
        eprintln!("refusing: {} already exists (reset flow: delete it, re-run, have an admin paste the new pubkey)", keypath.display());
        std::process::exit(1);
    }

    let signing = SigningKey::generate(&mut rand::rngs::OsRng);
    let pem = signing.to_pkcs8_pem(Default::default()).expect("encode pem");
    std::fs::write(&keypath, pem.as_bytes()).expect("write pem");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&keypath, std::fs::Permissions::from_mode(0o600));
    }
    println!("wrote private key: {}", keypath.display());

    // .iter/.gitignore must ignore everything under users/
    let gi_path = std::path::Path::new(".iter/.gitignore");
    let existing = std::fs::read_to_string(gi_path).unwrap_or_default();
    if !existing.lines().any(|l| l.trim() == "users/") {
        let mut content = existing;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("users/\n");
        std::fs::write(gi_path, content).expect("write .iter/.gitignore");
        println!("ensured .iter/.gitignore ignores users/");
    }

    let pubkey_b64 =
        base64::engine::general_purpose::STANDARD.encode(signing.verifying_key().to_bytes());
    println!("public key (base64): {pubkey_b64}");

    match api.post(&format!("/api/users/{name}/pubkey"), &json!({"pubkey": pubkey_b64})) {
        Ok(_) => println!("registered pubkey for '{name}' in iter_data"),
        Err(e) => println!(
            "could not register with iter_data ({e}); have an admin paste the pubkey via the users page"
        ),
    }
}

fn approve(api: &Api, workid_prefix: &str, pvtkeypath: Option<&str>, user: Option<&str>) {
    use ed25519_dalek::SigningKey;
    use ed25519_dalek::pkcs8::DecodePrivateKey;
    use ed25519_dalek::Signer;

    let keypath = pvtkeypath
        .map(String::from)
        .or_else(|| std::env::var("ITER_APPROVE_KEYPATH").ok())
        .unwrap_or_default();
    if keypath.is_empty() {
        eprintln!("no key: pass --pvtkeypath or set ITER_APPROVE_KEYPATH in your .env");
        std::process::exit(2);
    }
    let pem = std::fs::read_to_string(&keypath).unwrap_or_else(|e| {
        eprintln!("cannot read {keypath}: {e}");
        std::process::exit(2);
    });
    let signing = SigningKey::from_pkcs8_pem(&pem).unwrap_or_else(|e| {
        eprintln!("cannot parse {keypath}: {e}");
        std::process::exit(2);
    });
    let username = user
        .map(String::from)
        .or_else(|| {
            std::path::Path::new(&keypath)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
        })
        .unwrap_or_default();

    // find the workitem by id or unique prefix across visible projects
    let projects = api.get("/api/projects").ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let mut matches: Vec<(String, String)> = Vec::new();
    for p in &projects {
        let pname = p.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        if let Ok(items) = api.get(&format!("/api/projects/{pname}/workitems")) {
            for i in items.as_array().cloned().unwrap_or_default() {
                let id = i.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                if id.starts_with(workid_prefix) || id.replace('-', "").starts_with(workid_prefix) {
                    matches.push((pname.clone(), id));
                }
            }
        }
    }
    match matches.len() {
        0 => {
            eprintln!("no workitem matches '{workid_prefix}'");
            std::process::exit(1);
        }
        1 => {}
        n => {
            eprintln!("'{workid_prefix}' is ambiguous ({n} matches); use more of the id");
            std::process::exit(1);
        }
    }
    let (project, id) = matches.remove(0);
    let sig = signing.sign(id.as_bytes());
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
    match api.post(
        &format!("/api/projects/{project}/workitems/{id}/approve"),
        &json!({"user": username, "signature": sig_b64}),
    ) {
        Ok(_) => println!("approved {id} in '{project}' as {username}"),
        Err(e) => {
            eprintln!("approval rejected: {e}");
            std::process::exit(1);
        }
    }
}

fn accounts(api: &Api) {
    let projects = api.get("/api/projects").ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    for p in &projects {
        let pname = p.get("name").and_then(|n| n.as_str()).unwrap_or("");
        println!("project: {pname}");
        for a in p.get("accounts").and_then(|a| a.as_array()).cloned().unwrap_or_default() {
            let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let envar = a.get("token_envar").and_then(|n| n.as_str()).unwrap_or("");
            let set = std::env::var(envar).map(|v| !v.trim().is_empty()).unwrap_or(false);
            println!(
                "  {name}: {envar} = {}",
                if set { "SET" } else { "NOT SET (add to your .env)" }
            );
        }
    }
    if projects.is_empty() {
        println!("no projects visible (check token / data_url)");
    }
}
