use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::markers::{self, Role};

/// `iter validate` — deterministic checks (and light corrections) for `*.iter.md`
/// files, role-aware per the filename rule (markers::role_of).
///
/// Split by what a machine can safely do:
/// - FIXABLE (applied with --fix): purely mechanical normalizations whose result
///   is provably what the author meant — a `...` closing fence, junk before the
///   opening fence, an unquoted prose value containing ": " (the measured PyYAML
///   footgun from pdy work item 71123487).
/// - REPORT-ONLY: anything that needs a human/agent decision — missing keys,
///   missing files, filenames matching no role, stray identity keys.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Would break or mislead the engine — must be dealt with.
    Error,
    /// Works, but confusing or incomplete — should be dealt with.
    Warn,
    /// Worth knowing; often deliberate (e.g. no `testgroup:` key).
    Info,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub file: String,
    pub severity: Severity,
    /// Short machine-stable code, e.g. "unquoted-prose".
    pub code: &'static str,
    /// One-sentence human description of the exact problem.
    pub message: String,
    /// True when --fix repaired it in this run.
    pub fixed: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub files_checked: usize,
    pub findings: Vec<Finding>,
    pub fixed: usize,
}

impl Report {
    pub fn worst(&self) -> Option<Severity> {
        if self.findings.iter().any(|f| !f.fixed && f.severity == Severity::Error) {
            Some(Severity::Error)
        } else if self.findings.iter().any(|f| !f.fixed && f.severity == Severity::Warn) {
            Some(Severity::Warn)
        } else if self.findings.iter().any(|f| !f.fixed) {
            Some(Severity::Info)
        } else {
            None
        }
    }
}

/// Prose frontmatter keys that must be quoted when they contain a colon-space —
/// unquoted, strict-YAML readers see a nested key and refuse the whole block.
const PROSE_KEYS: &[&str] = &["name", "description", "endpoint"];

/// The four logical interaction kinds and each one's required H2 sections
/// (matched case-insensitively, trailing colons ignored). Every kind also
/// requires the tail sections, in order, closing the file.
fn interface_kind_sections(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "request-reply" => Some(&["Request", "Reply, success shape", "Reply, failure shape"]),
        "event" => Some(&["Event"]),
        "stream" => Some(&["Stream item", "Stream end"]),
        "dataset" => Some(&["Record"]),
        _ => None,
    }
}
const INTERFACE_TAIL_SECTIONS: &[&str] = &["Worked examples", "Invariants"];

/// Identity-bearing keys that belong to specific roles; anywhere else they are
/// harmless (the filename wins) but confuse readers.
fn stray_identity_keys(role: Option<Role>) -> &'static [&'static str] {
    match role {
        Some(Role::Marker) => &["interface", "participants"],
        Some(Role::Interface) => &["level", "participants"],
        Some(Role::Usecase) => &["level", "interface"],
        _ => &["level", "interface", "participants"],
    }
}

/// Validate one file. When `fix` is true the safe corrections are written back
/// (atomically) and reported with `fixed: true`.
pub fn validate_file(path: &Path, fix: bool) -> std::io::Result<Vec<Finding>> {
    let mut findings = Vec::new();
    let fname = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let file_s = path.to_string_lossy().into_owned();
    let original = std::fs::read_to_string(path)?;
    let mut text = original.clone();
    let role = markers::role_of(&fname);

    let mut push = |severity: Severity, code: &'static str, message: String, fixed: bool| {
        findings.push(Finding { file: file_s.clone(), severity, code, message, fixed });
    };

    // 1. Filename role.
    if role.is_none() {
        push(
            Severity::Warn,
            "no-role",
            "filename matches no role (marker/bizreq/techreq/interface/testgroup/usecase before .iter.md) — the engine treats it as a plain context doc; rename it if it was meant to be more".into(),
            false,
        );
    }

    // 2. Mechanical normalizations (fixable).
    // 2a. Junk before the opening fence (BOM, blank lines, spaces).
    let trimmed_start = text.trim_start_matches(['\u{feff}', ' ', '\t', '\n', '\r']);
    if trimmed_start.starts_with("---") && trimmed_start.len() != text.len() {
        push(
            Severity::Warn,
            "junk-before-fence",
            "whitespace or a byte-order mark sits before the opening `---` fence, hiding the frontmatter from strict parsers".into(),
            fix,
        );
        if fix {
            text = trimmed_start.to_string();
        }
    }
    // 2b. `...` used as the closing fence (YAML allows it; the engine does not).
    if let Some(rest) = text.strip_prefix("---\n") {
        let has_dash_close = rest.contains("\n---");
        let dots = rest.find("\n...");
        if !has_dash_close {
            if let Some(dot_pos) = dots {
                push(
                    Severity::Warn,
                    "dots-fence",
                    "frontmatter closes with `...` — the engine only recognizes `---`".into(),
                    fix,
                );
                if fix {
                    let abs = 4 + dot_pos; // offset of the "\n..." within `text`
                    text.replace_range(abs..abs + 4, "\n---");
                }
            } else {
                push(
                    Severity::Error,
                    "unterminated-frontmatter",
                    "the opening `---` fence never closes — the whole file reads as frontmatter and the engine sees no body and no keys".into(),
                    false,
                );
            }
        }
    }

    // Re-parse after mechanical fixes.
    let (front, participants, body) = markers::frontmatter(&text);
    let has_frontmatter = text.trim_start().starts_with("---") && !front.is_empty();

    // 2c. Unquoted prose values containing ": " (fixable) — measured to make
    // PyYAML refuse 18 of 37 blocks (pdy work item 71123487). Only frontmatter
    // lines are touched: the region between the opening fence and the first
    // closing fence — body prose that happens to start with `name:` is left alone.
    if has_frontmatter {
        let close_line = text
            .lines()
            .enumerate()
            .skip(1)
            .find(|(_, l)| l.trim() == "---")
            .map(|(i, _)| i)
            .unwrap_or(0);
        let mut fixed_lines = Vec::new();
        let rebuilt: Vec<String> = text
            .lines()
            .enumerate()
            .map(|(i, line)| {
                if i == 0 || i >= close_line {
                    return line.to_string();
                }
                let t = line.trim_start();
                for key in PROSE_KEYS {
                    if let Some(raw) = t.strip_prefix(&format!("{}:", key)) {
                        let val = raw.trim();
                        let quoted = (val.starts_with('"') && val.ends_with('"'))
                            || (val.starts_with('\'') && val.ends_with('\''));
                        if !quoted && val.contains(": ") && !val.contains('"') {
                            fixed_lines.push(format!("{}: {}", key, val));
                            if fix {
                                let indent = &line[..line.len() - t.len()];
                                return format!("{}{}: \"{}\"", indent, key, val);
                            }
                        }
                    }
                }
                line.to_string()
            })
            .collect();
        for l in &fixed_lines {
            push(
                Severity::Error,
                "unquoted-prose",
                format!("`{}` contains a colon-space and is unquoted — strict YAML readers refuse the whole block; quote the value", l),
                fix,
            );
        }
        if fix && !fixed_lines.is_empty() {
            let mut joined = rebuilt.join("\n");
            if text.ends_with('\n') {
                joined.push('\n');
            }
            text = joined;
        }
    }

    // 3. Role-specific expectations (report-only).
    match role {
        Some(Role::Marker) => {
            if !has_frontmatter {
                push(
                    Severity::Error,
                    "missing-frontmatter",
                    "a marker file needs `---`-fenced frontmatter (name, level, description) — without it the node has no name or level on the Projects map".into(),
                    false,
                );
            } else {
                for (key, sev) in [("name", Severity::Warn), ("level", Severity::Error), ("description", Severity::Warn)] {
                    if front.get(key).map(|v| v.trim().is_empty()).unwrap_or(true) {
                        push(sev, "missing-key", format!("marker frontmatter has no `{}:` — the Projects map shows a fallback or nothing", key), false);
                    }
                }
                if let Some(level) = front.get("level") {
                    if !["project", "context", "container", "component"].contains(&level.as_str()) {
                        push(Severity::Error, "bad-level", format!("`level: {}` is not one of project|context|container|component", level), false);
                    }
                }
                match front.get("testgroup") {
                    None => push(
                        Severity::Info,
                        "no-testgroup-key",
                        "no `testgroup:` key — this C4 object is deliberately outside the test sweep (declare the key if tests should exist)".into(),
                        false,
                    ),
                    Some(tg) => {
                        let tg_path = path.parent().unwrap_or(Path::new(".")).join(tg.trim());
                        if !tg_path.is_file() {
                            push(Severity::Error, "declared-testgroup-missing", format!("`testgroup: {}` is declared but the file does not exist — a testwriter item should create it", tg), false);
                        }
                        if front.get("test_dir").map(|v| v.trim().is_empty()).unwrap_or(true) {
                            push(Severity::Warn, "missing-key", "`testgroup:` is declared but `test_dir:` is not — the lock carve for parallel code/testwriter items falls back to a guess".into(), false);
                        }
                    }
                }
                if !body.to_lowercase().contains("long description") {
                    push(Severity::Info, "no-long-description", "marker body has no `# Long Description` section (`iter stubdesc` can stub it; agents should write it for real)".into(), false);
                }
            }
        }
        Some(Role::Interface) => {
            if front.get("interface").map(|v| v.trim().is_empty()).unwrap_or(true) {
                push(Severity::Error, "missing-key", "interface file has no `interface:` id — nothing can reference it from uses:/provides:".into(), false);
            }
            let kind = front.get("kind").map(|v| v.trim().to_string()).unwrap_or_default();
            let kind_sections = interface_kind_sections(&kind);
            if kind.is_empty() {
                push(Severity::Warn, "missing-key", "interface file has no `kind:` (request-reply | event | stream | dataset)".into(), false);
            } else if kind_sections.is_none() {
                push(Severity::Warn, "bad-kind", format!("`kind: {}` is not a logical interaction kind (request-reply | event | stream | dataset) — transports and formats are not kinds", kind), false);
            }
            // The body IS the contract, in the fixed format: an H1 title, a
            // summary under 300 chars, the kind's required H2 sections, then
            // `## Worked examples` (strict JSON) and `## Invariants` last.
            let contract = body.trim();
            if contract.is_empty() {
                push(
                    Severity::Warn,
                    "empty-contract-body",
                    "interface body is empty — the body IS the contract (get the skeleton: `iter validate --file <this file> --template`)".into(),
                    false,
                );
            } else {
                // Walk ``` fences: collect (language-tag, content, owning H2
                // section) per block, headings and summary from outside lines.
                let mut in_fence = false;
                let mut fence_lang = String::new();
                let mut block = String::new();
                let mut blocks: Vec<(String, String, String)> = Vec::new();
                let mut outside: Vec<&str> = Vec::new();
                let mut sections: Vec<String> = Vec::new();
                let mut cur_section = String::new();
                let mut h1_count = 0usize;
                let mut summary_len = 0usize;
                for line in contract.lines() {
                    let t = line.trim_start();
                    if t.starts_with("```") {
                        if in_fence {
                            blocks.push((fence_lang.clone(), std::mem::take(&mut block), cur_section.clone()));
                        } else {
                            fence_lang = t.trim_start_matches('`').trim().to_lowercase();
                        }
                        in_fence = !in_fence;
                        continue;
                    }
                    if in_fence {
                        block.push_str(line);
                        block.push('\n');
                        continue;
                    }
                    outside.push(line);
                    if let Some(h) = t.strip_prefix("## ") {
                        let name = h.trim().trim_end_matches(':').trim().to_string();
                        sections.push(name.clone());
                        cur_section = name;
                    } else if t.starts_with("# ") {
                        h1_count += 1;
                    } else if h1_count > 0 && sections.is_empty() && !t.is_empty() {
                        summary_len += t.len() + 1; // prose between the H1 and the first H2
                    }
                }
                if in_fence {
                    push(
                        Severity::Warn,
                        "unclosed-fence",
                        "a ``` fence never closes — everything after it reads as part of the example".into(),
                        false,
                    );
                } else if blocks.is_empty() {
                    push(
                        Severity::Warn,
                        "no-example-block",
                        "interface body has no fenced example block — show the message shapes as fenced examples, not prose about them".into(),
                        false,
                    );
                }
                // H1 + summary.
                if h1_count == 0 {
                    push(Severity::Warn, "missing-summary", "no `# <id> — contract` H1 title; the fixed format is H1 + a summary under 300 characters, then the H2 sections".into(), false);
                } else if h1_count > 1 {
                    push(Severity::Warn, "multiple-h1", format!("{} H1 headings — the fixed format has exactly one, followed by the summary", h1_count), false);
                } else if summary_len == 0 {
                    push(Severity::Warn, "missing-summary", "no summary prose between the H1 title and the first H2 section".into(), false);
                } else if summary_len > 300 {
                    push(Severity::Warn, "summary-too-long", format!("the summary between the H1 and the first H2 is {} characters — the cap is 300; move detail into the sections", summary_len), false);
                }
                // Required and allowed H2 sections, per kind.
                if let Some(required) = kind_sections {
                    let has = |name: &str| sections.iter().any(|s| s.eq_ignore_ascii_case(name));
                    for name in required.iter().chain(INTERFACE_TAIL_SECTIONS) {
                        if !has(name) {
                            push(Severity::Warn, "missing-section", format!("`kind: {}` requires a `## {}` section and the body has none", kind, name), false);
                        }
                    }
                    for s in &sections {
                        let allowed = required.iter().chain(INTERFACE_TAIL_SECTIONS).any(|n| s.eq_ignore_ascii_case(n));
                        if !allowed {
                            push(Severity::Warn, "unexpected-section", format!("`## {}` is not a section of the fixed format for `kind: {}` — the contract holds ONLY the format's sections; other prose belongs on a marker or techreq", s, kind), false);
                        }
                    }
                    // Tail order: Worked examples, then Invariants, closing the file.
                    let tail_ok = sections.len() >= 2
                        && sections[sections.len() - 2].eq_ignore_ascii_case("Worked examples")
                        && sections[sections.len() - 1].eq_ignore_ascii_case("Invariants");
                    if !tail_ok && INTERFACE_TAIL_SECTIONS.iter().all(|n| has(n)) {
                        push(Severity::Warn, "section-order", "`## Worked examples` then `## Invariants` must be the last two sections".into(), false);
                    }
                    // Worked examples must be machine-checkable: a strict-JSON fence.
                    let we_json = blocks.iter().any(|(lang, _, sec)| lang == "json" && sec.eq_ignore_ascii_case("Worked examples"));
                    if has("Worked examples") && !we_json {
                        push(Severity::Warn, "worked-examples-not-json", "the `## Worked examples` section has no ```json fence — worked examples are normative and must strictly parse".into(), false);
                    }
                }
                for (lang, content, _) in &blocks {
                    if lang == "json" && serde_json::from_str::<serde_json::Value>(content).is_err() {
                        push(
                            Severity::Warn,
                            "bad-json-example",
                            "an example fence is tagged `json` but does not parse as strict JSON — fix the example or untag the fence (pseudo-JSON stays untagged)".into(),
                            false,
                        );
                    }
                }
                // WHAT, never WHO/HOW: usage phrasing outside the example blocks.
                const USAGE_PHRASES: &[&str] = &[
                    "used by", "consumed by", "called by", "caller:", "callers:",
                    "consumer:", "consumers:", "provider:", "providers:", "## owns", "## consumes",
                ];
                let mut hits: Vec<&str> = Vec::new();
                for line in &outside {
                    let low = line.to_lowercase();
                    for p in USAGE_PHRASES {
                        if low.contains(p) && !hits.contains(p) {
                            hits.push(p);
                        }
                    }
                }
                if !hits.is_empty() {
                    push(
                        Severity::Warn,
                        "usage-in-contract",
                        format!(
                            "usage details in the contract body (\"{}\") — an interface file records WHAT crosses the boundary, never who provides/consumes it or how; marker `provides:`/`uses:` keys carry that",
                            hits.join("\", \"")
                        ),
                        false,
                    );
                }
            }
        }
        Some(Role::Usecase) => {
            if front.get("name").map(|v| v.trim().is_empty()).unwrap_or(true) {
                push(Severity::Warn, "missing-key", "usecase file has no `name:` — it lists blank in the Projects view".into(), false);
            }
            if participants.is_empty() {
                push(Severity::Info, "no-participants", "no `participants:` lines — the use-case exists but threads nothing through the tree".into(), false);
            }
        }
        Some(Role::Testgroup) => {
            if !text.contains(crate::testgroups::BLOCK_START) {
                push(Severity::Warn, "no-testgroups-block", "no `<!-- iterapp:testgroups -->` block — no groups are defined here yet".into(), false);
            } else {
                for group in crate::testgroups::parse(&text) {
                    for t in &group.testlist {
                        let script = path.parent().unwrap_or(Path::new(".")).join(&t.shell);
                        if !script.is_file() {
                            push(
                                Severity::Error,
                                "missing-test-script",
                                format!("group \"{}\" registers `{}` but the script does not exist", group.label, t.shell),
                                false,
                            );
                        }
                    }
                }
            }
        }
        _ => {}
    }

    // 4. Stray identity keys: harmless (the filename wins) but confusing.
    if has_frontmatter {
        for key in stray_identity_keys(role) {
            let present = if *key == "participants" { !participants.is_empty() } else { front.contains_key(*key) };
            if present {
                push(
                    Severity::Warn,
                    "stray-identity-key",
                    format!("`{}:` has no meaning in a {} file — the filename decides the role; remove the key or rename the file", key, role_name(role)),
                    false,
                );
            }
        }
    }

    if fix && text != original {
        let tmp = path.with_extension("md.tmp");
        std::fs::write(&tmp, &text)?;
        std::fs::rename(&tmp, path)?;
    }
    Ok(findings)
}

/// `iter validate --file <path> --template` — the one authoritative skeleton
/// per role, stubs in <angle brackets>. Agents fetch this instead of carrying
/// every file template in context; changing a template here changes it for
/// every agent at once. The role comes from the FILENAME; interface skeletons
/// follow the file's existing `kind:` when it already declares a valid one,
/// and stub `request-reply` otherwise.
pub fn template_for(path: &Path) -> Result<String, String> {
    let fname = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let Some(role) = markers::role_of(&fname) else {
        return Err(format!(
            "`{}` matches no role (marker/bizreq/techreq/interface/testgroup/usecase before .iter.md) — a plain context doc has no template",
            fname
        ));
    };
    Ok(match role {
        Role::Interface => {
            let declared = std::fs::read_to_string(path)
                .ok()
                .map(|t| markers::frontmatter(&t).0.get("kind").cloned().unwrap_or_default())
                .unwrap_or_default();
            let kind = if interface_kind_sections(&declared).is_some() { declared } else { "request-reply".to_string() };
            interface_template(&kind)
        }
        Role::Marker => MARKER_TEMPLATE.trim_start().to_string(),
        Role::Bizreq => req_template("BIZ", "business requirements", "WHAT the business needs — never how it is built"),
        Role::Techreq => req_template("TECH", "technical requirements", "a technical constraint the build must honor"),
        Role::Testgroup => TESTGROUP_TEMPLATE.trim_start().to_string(),
        Role::Usecase => USECASE_TEMPLATE.trim_start().to_string(),
    })
}

fn interface_template(kind: &str) -> String {
    let middle = match kind {
        "event" => {
            "## Event\n\n```\n{\n  \"<field>\": \"<example>\"        // <type>, <required|optional — default>\n}\n```\n\n## Worked examples\n\nNormative — each event must be producible and acceptable on every implementation (strict JSON):\n\n```json\n[\n  { \"event\": { } }\n]\n```\n"
        }
        "stream" => {
            "## Stream item\n\n```\n{\n  \"<field>\": \"<example>\"        // <type>; state the ordering rule items arrive in\n}\n```\n\n## Stream end\n\n```\n{\n  \"end\": {\n    \"reason\": \"<COMPLETE | closed vocabulary of failure codes>\"\n  }\n}\n```\n\n## Worked examples\n\nNormative — each sequence must hold on every implementation (strict JSON):\n\n```json\n[\n  { \"items\": [ { } ], \"end\": { \"reason\": \"COMPLETE\" } }\n]\n```\n"
        }
        "dataset" => {
            "## Record\n\n```\n{\n  \"<field>\": \"<example>\"        // <type>; mark the identity/key fields\n}\n```\n\n## Worked examples\n\nNormative — each record must be producible and acceptable on every implementation (strict JSON):\n\n```json\n[\n  { \"record\": { } }\n]\n```\n"
        }
        _ => {
            "## Request\n\n```\n{\n  \"<field>\": \"<example>\"        // <type>, <required|optional — default>\n}\n```\n\n## Reply, success shape\n\n```\n{\n  \"<field>\": \"<example>\"        // <type, rules the value must satisfy>\n}\n```\n\n## Reply, failure shape\n\n```\n{\n  \"refusal\": {\n    \"code\":   \"<REFUSAL_CODE>\",  // closed vocabulary — list every code\n    \"detail\": \"<one line naming what was refused>\"\n  }\n}\n```\n\n## Worked examples\n\nNormative — each pair must hold on every implementation (strict JSON):\n\n```json\n[\n  { \"request\": { }, \"reply\": { } }\n]\n```\n"
        }
    };
    format!(
        "---\ninterface: <kebab-case-id>\nkind: {}                    # request-reply | event | stream | dataset\ndescription: \"<one line: what data crosses this boundary>\"\n---\n\n# <kebab-case-id> — contract\n\n<Named summary, under 300 characters: what goes in, what comes out, and why.\nNo carrier, no consumers, no deployment.>\n\n{}\n## Invariants\n\n- <property the examples cannot show: totality, determinism, ordering, limits,\n  closed vocabularies>\n- Transport-neutral: these messages ride any carrier unchanged; carrier\n  bindings (routes, ports, topics, flags, exit codes) live on the serving\n  object's marker, never here.\n",
        kind, middle
    )
}

fn req_template(prefix: &str, title: &str, statement: &str) -> String {
    format!(
        "# <component> — {}\n\n- **<PREFIX>-{}-001** — <one requirement per bullet, stable id never renumbered,\n  a testable statement of {}.>\n",
        title, prefix, statement
    )
}

const MARKER_TEMPLATE: &str = r#"
---
name: "<Human-Readable Name>"
level: component                # project | context | container | component
description: "<one line on what this C4 object is>"
uses: [<interface-id>]          # interfaces it consumes (optional)
provides: [<interface-id>]      # interfaces it serves (optional)
testgroup: tests/testgroup.iter.md   # THIS object's testgroup file, relative to this marker
test_dir: tests                 # subtree holding its test scripts
---

# Long Description

<Plain-language description for a non-technical reader — describe, don't
state; no jargon; define every acronym on first use; link related project
parts by their marker path. Never leave TBD.>

## <interface-id> binding

<Only if this object serves or consumes an interface over a specific carrier:
how the transport-neutral messages map to that carrier — routes, topics,
flags, exit codes. One short block per bound interface.>
"#;

const TESTGROUP_TEMPLATE: &str = r#"
# <component> — test groups

<!-- iterapp:testgroups
{"label":"<Group label>","testlist":[{"id":"t1","name":"<short name>","desc":"<what it proves>","shell":"tests/iter/t1.sh"}]}
-->
"#;

const USECASE_TEMPLATE: &str = r#"
---
name: "<Use-case name>"
description: "<one line on the thread>"
participants:
  - 1 <marker key of the first participant>
  - 2 <marker key of the next participant>
---

# <Use-case name>

<The narrative: who initiates, what flows through which participants, what
comes out. Reference interfaces by id and C4 objects by marker path.>
"#;

fn role_name(role: Option<Role>) -> &'static str {
    match role {
        Some(Role::Marker) => "marker",
        Some(Role::Bizreq) => "bizreq",
        Some(Role::Techreq) => "techreq",
        Some(Role::Interface) => "interface",
        Some(Role::Testgroup) => "testgroup",
        Some(Role::Usecase) => "usecase",
        None => "plain (no-role)",
    }
}

/// Validate one file or every `*.iter.md` under the given roots.
pub fn run(roots: &[PathBuf], single: Option<&Path>, fix: bool) -> std::io::Result<Report> {
    let mut report = Report::default();
    let files: Vec<PathBuf> = match single {
        Some(f) => vec![f.to_path_buf()],
        None => {
            let mut out = Vec::new();
            for root in roots {
                collect_iter_files(root, &mut out);
            }
            out.sort();
            out.dedup();
            out
        }
    };
    for file in files {
        report.files_checked += 1;
        match validate_file(&file, fix) {
            Ok(fs) => {
                report.fixed += fs.iter().filter(|f| f.fixed).count();
                report.findings.extend(fs);
            }
            Err(e) => report.findings.push(Finding {
                file: file.to_string_lossy().into_owned(),
                severity: Severity::Error,
                code: "unreadable",
                message: format!("cannot read: {}", e),
                fixed: false,
            }),
        }
    }
    Ok(report)
}

fn collect_iter_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if matches!(name.as_str(), ".git" | "target" | "node_modules") {
                continue;
            }
            collect_iter_files(&path, out);
        } else if name.ends_with(".iter.md") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iter-validate-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn codes(fs: &[Finding]) -> Vec<&'static str> {
        fs.iter().map(|f| f.code).collect()
    }

    #[test]
    fn clean_marker_passes_with_info_only() {
        let dir = tmp("clean");
        std::fs::create_dir_all(dir.join("test")).unwrap();
        std::fs::write(dir.join("test/testgroup.iter.md"), "# tests\n").unwrap();
        let p = dir.join("comp.marker.iter.md");
        std::fs::write(&p, "---\nname: \"Comp\"\nlevel: component\ndescription: \"a thing\"\ntestgroup: test/testgroup.iter.md\ntest_dir: test\n---\n# Long Description\nreal words\n").unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(fs.iter().all(|f| f.severity == Severity::Info), "{:?}", fs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unquoted_prose_is_flagged_and_fixed() {
        let dir = tmp("quote");
        let p = dir.join("comp.marker.iter.md");
        std::fs::write(&p, "---\nname: Comp\nlevel: component\ndescription: svc-intake: thin host for m08\n---\nbody\n").unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(codes(&fs).contains(&"unquoted-prose"), "{:?}", fs);
        assert!(!fs.iter().find(|f| f.code == "unquoted-prose").unwrap().fixed);

        let fs = validate_file(&p, true).unwrap();
        assert!(fs.iter().find(|f| f.code == "unquoted-prose").unwrap().fixed);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("description: \"svc-intake: thin host for m08\""), "{}", text);
        // Idempotent: a second pass finds nothing to quote.
        let fs = validate_file(&p, true).unwrap();
        assert!(!codes(&fs).contains(&"unquoted-prose"), "{:?}", fs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dots_fence_and_leading_junk_are_fixed() {
        let dir = tmp("fence");
        let p = dir.join("x.marker.iter.md");
        std::fs::write(&p, "\n\n---\nname: \"X\"\nlevel: component\ndescription: \"d\"\n...\nbody\n").unwrap();
        let fs = validate_file(&p, true).unwrap();
        let c = codes(&fs);
        assert!(c.contains(&"junk-before-fence") && c.contains(&"dots-fence"), "{:?}", fs);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.starts_with("---\n") && text.contains("\n---\nbody"), "{}", text);
        // The engine can now parse it.
        let (front, _, _) = markers::frontmatter(&text);
        assert_eq!(front.get("name").map(String::as_str), Some("X"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unterminated_frontmatter_is_an_error_not_a_guessed_fix() {
        let dir = tmp("unterminated");
        let p = dir.join("x.marker.iter.md");
        let body = "---\nname: \"X\"\nlevel: component\nno closing fence anywhere\n";
        std::fs::write(&p, body).unwrap();
        let fs = validate_file(&p, true).unwrap();
        assert!(codes(&fs).contains(&"unterminated-frontmatter"), "{:?}", fs);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body, "no guessed rewrite");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn role_specific_reports() {
        let dir = tmp("roles");
        // Marker: missing level + declared-but-missing testgroup file.
        let m = dir.join("m.marker.iter.md");
        std::fs::write(&m, "---\nname: \"M\"\ndescription: \"d\"\ntestgroup: test/testgroup.iter.md\n---\nbody").unwrap();
        let fs = validate_file(&m, false).unwrap();
        let c = codes(&fs);
        assert!(c.contains(&"missing-key") && c.contains(&"declared-testgroup-missing"), "{:?}", fs);

        // Interface without an id.
        let i = dir.join("api.interface.iter.md");
        std::fs::write(&i, "---\nkind: http\n---\ncontract").unwrap();
        assert!(codes(&validate_file(&i, false).unwrap()).contains(&"missing-key"));

        // Stray level: in a usecase file — filename wins, key flagged.
        let u = dir.join("flow.usecase.iter.md");
        std::fs::write(&u, "---\nname: \"F\"\nlevel: container\nparticipants:\n  - 1 core\n---\nstory").unwrap();
        assert!(codes(&validate_file(&u, false).unwrap()).contains(&"stray-identity-key"));

        // No-role filename.
        let n = dir.join("notes.iter.md");
        std::fs::write(&n, "just notes\n").unwrap();
        assert!(codes(&validate_file(&n, false).unwrap()).contains(&"no-role"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fully format-compliant request-reply contract.
    const CLEAN_IFACE: &str = "---\ninterface: pay-msg\nkind: request-reply\ndescription: \"a payment in, a receipt or refusal out\"\n---\n\n# pay-msg — contract\n\nA payment request in; exactly one reply out — a receipt XOR a refusal.\n\n## Request\n\n```\n{ \"amount_cents\": 1200 }\n```\n\n## Reply, success shape\n\n```\n{ \"receipt_id\": \"r-1\" }\n```\n\n## Reply, failure shape\n\n```\n{ \"refusal\": { \"code\": \"NO_FUNDS\" } }\n```\n\n## Worked examples\n\n```json\n[ { \"request\": { \"amount_cents\": 1200 }, \"reply\": { \"receipt_id\": \"r-1\" } } ]\n```\n\n## Invariants\n\n- Deterministic and total.\n";

    #[test]
    fn interface_body_is_the_contract() {
        let dir = tmp("ifacebody");
        let p = dir.join("api.interface.iter.md");

        // Clean: the fixed format end to end → no findings at all.
        std::fs::write(&p, CLEAN_IFACE).unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(fs.is_empty(), "{:?}", fs);

        // Empty body.
        std::fs::write(&p, "---\ninterface: pay-msg\nkind: request-reply\n---\n").unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"empty-contract-body"));

        // Prose but no fenced example.
        std::fs::write(&p, "---\ninterface: pay-msg\nkind: request-reply\n---\ntwo pages of prose about the payload\n").unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"no-example-block"));

        // Tagged json that does not parse; pseudo-JSON untagged is fine.
        std::fs::write(&p, "---\ninterface: pay-msg\nkind: request-reply\n---\n```json\n{ \"amount\": … }\n```\n").unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"bad-json-example"));
        std::fs::write(&p, "---\ninterface: pay-msg\nkind: request-reply\n---\n```\n{ \"amount\": … }\n```\n").unwrap();
        assert!(!codes(&validate_file(&p, false).unwrap()).contains(&"bad-json-example"));

        // Unclosed fence.
        std::fs::write(&p, "---\ninterface: pay-msg\nkind: request-reply\n---\n```json\n{ \"a\": 1 }\n").unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"unclosed-fence"));

        // Usage prose outside the fence is flagged; the same words INSIDE an
        // example (e.g. a kafka `consumer:` config key) are not.
        std::fs::write(&p, "---\ninterface: pay-msg\nkind: request-reply\n---\nThis is consumed by the settlement service.\n```json\n{ \"a\": 1 }\n```\n").unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(codes(&fs).contains(&"usage-in-contract"), "{:?}", fs);
        std::fs::write(&p, "---\ninterface: pay-msg\nkind: request-reply\n---\n```\nconsumer: settlement-group\n```\n").unwrap();
        assert!(!codes(&validate_file(&p, false).unwrap()).contains(&"usage-in-contract"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn interface_fixed_format_checks() {
        let dir = tmp("ifaceformat");
        let p = dir.join("api.interface.iter.md");

        // Transport/format kinds are no longer kinds.
        std::fs::write(&p, CLEAN_IFACE.replace("kind: request-reply", "kind: grpc")).unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"bad-kind"));

        // Missing a required section for the kind.
        std::fs::write(&p, CLEAN_IFACE.replace("## Reply, failure shape", "## Whatever")).unwrap();
        let c = codes(&validate_file(&p, false).unwrap());
        assert!(c.contains(&"missing-section") && c.contains(&"unexpected-section"), "{:?}", c);

        // Tail order: Invariants must close the file, after Worked examples.
        let swapped = CLEAN_IFACE
            .replace("## Worked examples", "## Invariants")
            .replacen("## Invariants\n\n- Deterministic and total.\n", "## Worked examples\n\n```json\n[]\n```\n", 1);
        std::fs::write(&p, swapped).unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"section-order"));

        // Worked examples without a strict-JSON fence.
        std::fs::write(&p, CLEAN_IFACE.replace("```json", "```")).unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"worked-examples-not-json"));

        // No H1, and a summary over 300 characters.
        std::fs::write(&p, CLEAN_IFACE.replace("# pay-msg — contract\n\n", "")).unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"missing-summary"));
        let long = CLEAN_IFACE.replace(
            "A payment request in; exactly one reply out — a receipt XOR a refusal.",
            &"words ".repeat(60),
        );
        std::fs::write(&p, long).unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"summary-too-long"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn templates_match_the_validator() {
        let dir = tmp("templates");

        // The request-reply skeleton names every section the validator requires.
        let p = dir.join("new.interface.iter.md");
        let t = template_for(&p).unwrap();
        for section in ["## Request", "## Reply, success shape", "## Reply, failure shape", "## Worked examples", "## Invariants"] {
            assert!(t.contains(section), "missing {} in:\n{}", section, t);
        }

        // An existing file's kind: steers the skeleton.
        std::fs::write(&p, "---\ninterface: x\nkind: event\n---\n").unwrap();
        let t = template_for(&p).unwrap();
        assert!(t.contains("## Event") && !t.contains("## Request"), "{}", t);

        // Every role has a template; a no-role filename is an error.
        for f in ["a.marker.iter.md", "a.bizreq.iter.md", "a.techreq.iter.md", "a.testgroup.iter.md", "a.usecase.iter.md"] {
            assert!(template_for(&dir.join(f)).is_ok(), "{}", f);
        }
        assert!(template_for(&dir.join("notes.iter.md")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn testgroup_missing_script_is_an_error() {
        let dir = tmp("tgscripts");
        let p = dir.join("testgroup.iter.md");
        std::fs::write(&p, "# t\n<!-- iterapp:testgroups\n{\"label\":\"G\",\"testlist\":[{\"id\":\"t1\",\"name\":\"t\",\"desc\":\"\",\"shell\":\"tests/iter/t1.sh\"}]}\n-->\n").unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(codes(&fs).contains(&"missing-test-script"), "{:?}", fs);
        std::fs::create_dir_all(dir.join("tests/iter")).unwrap();
        std::fs::write(dir.join("tests/iter/t1.sh"), "exit 0\n").unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(!codes(&fs).contains(&"missing-test-script"), "{:?}", fs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_walks_roots_and_counts() {
        let dir = tmp("walk");
        std::fs::write(dir.join("a.marker.iter.md"), "---\nname: \"A\"\nlevel: component\ndescription: \"d\"\n---\n# Long Description\nx\n").unwrap();
        std::fs::write(dir.join("notes.iter.md"), "plain\n").unwrap();
        let report = run(&[dir.clone()], None, false).unwrap();
        assert_eq!(report.files_checked, 2);
        assert_eq!(report.worst(), Some(Severity::Warn), "{:?}", report.findings); // no-role warn
        let _ = std::fs::remove_dir_all(&dir);
    }
}
