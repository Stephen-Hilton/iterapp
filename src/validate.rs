use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::markers::{self, Role};

/// `iter validate` — deterministic checks (and light corrections) for `*.iter.md`
/// files, role-aware per the structureV2 filename dot rule (markers::role_of).
///
/// Split by what a machine can safely do:
/// - FIXABLE (applied with --fix): purely mechanical normalizations whose result
///   is provably what the author meant — a `...` closing fence, junk before the
///   opening fence, an unquoted prose value containing ": ".
/// - REPORT-ONLY: anything that needs a human/agent decision — missing keys,
///   V1-style filenames, unknown children sub-keys, missing files.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Would break or mislead the engine — must be dealt with.
    Error,
    /// Works, but confusing or incomplete — should be dealt with.
    Warn,
    /// Worth knowing; often deliberate.
    Info,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub file: String,
    pub severity: Severity,
    pub code: &'static str,
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
const PROSE_KEYS: &[&str] = &["name", "description", "endpoint", "projectname", "projectdescription"];

/// The children sub-keys each nodetype accepts (structureV2 "children
/// sub-keys, by nodetype").
fn children_keys(role: Role) -> &'static [&'static str] {
    match role {
        Role::Code => &["codenodes", "codedirs", "inputs", "outputs", "bizreqs", "techreqs", "testgroups"],
        Role::Bizreq | Role::Techreq => &["reqpaths"],
        Role::Interface => &["bizreqs", "techreqs", "testgroups"],
        Role::Usecase => &["codenodes", "testgroups"],
        Role::Testgroup => &["testpaths"],
        Role::Main => &[],
    }
}

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
/// The one OPTIONAL section, accepted on every kind and only as the file's last
/// section, after `## Invariants`: a declared deviation from the internal
/// transport law.
const INTERFACE_OPTIONAL_TAIL_SECTION: &str = "Exceptions";

/// V1 role words: a stem ending in one of these WITHOUT the dot separator is
/// almost certainly an unmigrated V1 file, worth a targeted message.
const V1_STYLE_TAILS: &[&str] = &["marker", "code", "bizreq", "techreq", "interface", "testgroup", "usecase", "main"];

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

    // 1. Filename rule (the V2 dot rule).
    if role.is_none() {
        let stem = fname.strip_suffix(".iter.md").unwrap_or(&fname).to_lowercase();
        if stem.ends_with("marker") {
            push(
                Severity::Error,
                "v1-filename",
                "V1 `*marker.iter.md` files are dead — the nodetype is `code` now and needs the dot rule: rename to `<prefix>.code.iter.md` (run the V2 migration)".into(),
                false,
            );
        } else if let Some(tail) = V1_STYLE_TAILS.iter().find(|t| stem.ends_with(**t) && stem.len() > t.len()) {
            push(
                Severity::Warn,
                "v1-filename",
                format!(
                    "filename ends in \"{tail}\" without the dot separator — the V2 rule is `*.{tail}.iter.md` (a dot before the nodetype unless the file has no prefix); as-is this is a plain context doc",
                ),
                false,
            );
        } else {
            push(
                Severity::Info,
                "no-role",
                "filename matches no nodetype (`*.main|code|bizreq|techreq|interface|testgroup|usecase.iter.md`) — the engine treats it as a plain context doc; rename it if it was meant to be more".into(),
                false,
            );
        }
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
    let front = markers::parse_front(&text);
    let has_frontmatter = front.has_frontmatter;
    let body = front.body.clone();

    // 2c. Unquoted prose values containing ": " (fixable). Only frontmatter
    // lines are touched: the region between the opening fence and the first
    // closing fence.
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

    // teststate vocabulary: a typo'd value silently means INCLUDED — the exact
    // wrong failure mode for a parking flag.
    if matches!(role, Some(Role::Code) | Some(Role::Usecase) | Some(Role::Interface)) {
        if front.scalars.contains_key("test_loop") {
            push(
                Severity::Warn,
                "legacy-key",
                "`test_loop:` is the V1 key — V2 is `teststate:` (values omit|include|block|inherit; `blocked` became `block`); the engine still reads it, the migration rewrites it".into(),
                false,
            );
        }
        let v = front.teststate();
        if !v.is_empty() && !matches!(v.as_str(), "omit" | "include" | "block" | "inherit") {
            push(
                Severity::Warn,
                "bad-teststate",
                format!("`teststate: {}` is not omit|include|block|inherit — an unrecognized value silently means INCLUDED in the sweep", v),
                false,
            );
        }
        if let Some(owner) = front.scalars.get("owner") {
            if !matches!(owner.trim(), "bespoke" | "oss" | "3rdparty") {
                push(
                    Severity::Warn,
                    "bad-owner",
                    format!("`owner: {}` is not bespoke|oss|3rdparty", owner.trim()),
                    false,
                );
            }
        }
    }

    // 3. Common structureV2 frontmatter law: EVERY nodetype requires
    // frontmatter with name, description, and a children mapping with at
    // least one sub-key (bizreq/techreq may declare `reqpaths: []` when the
    // body holds the requirements).
    if let Some(role) = role {
        if !has_frontmatter {
            push(
                Severity::Error,
                "missing-frontmatter",
                format!(
                    "a {} file needs `---`-fenced frontmatter (name, description, children) — structureV2 requires it on every nodetype",
                    markers::role_name(Some(role))
                ),
                false,
            );
        } else {
            let (name_key, desc_key) = if role == Role::Main {
                ("projectname", "projectdescription")
            } else {
                ("name", "description")
            };
            for (key, sev) in [(name_key, Severity::Warn), (desc_key, Severity::Warn)] {
                if front.scalar(key).trim().is_empty() {
                    push(sev, "missing-key", format!("frontmatter has no `{}:` — the UI shows a fallback or nothing", key), false);
                }
            }
            if role != Role::Main {
                if !front.children_present {
                    push(
                        Severity::Warn,
                        "missing-children",
                        "no `children:` mapping — every node declares its links (write the defaults out explicitly; body-only bizreq/techreq may use `reqpaths: []`)".into(),
                        false,
                    );
                } else {
                    let allowed = children_keys(role);
                    for key in front.children.keys() {
                        if !allowed.contains(&key.as_str()) {
                            push(
                                Severity::Warn,
                                "unknown-child-key",
                                format!(
                                    "`children.{}:` is not a {} sub-key (valid: {})",
                                    key,
                                    markers::role_name(Some(role)),
                                    allowed.join(", ")
                                ),
                                false,
                            );
                        }
                    }
                    if front.children.is_empty() {
                        push(
                            Severity::Warn,
                            "missing-children",
                            "`children:` is empty — declare at least one sub-key".into(),
                            false,
                        );
                    }
                }
            }
        }
    }

    // 4. Role-specific expectations (report-only).
    match role {
        Some(Role::Code) => {
            if has_frontmatter {
                let level = front.scalar("level");
                if level.trim().is_empty() {
                    push(Severity::Error, "missing-key", "code frontmatter has no `level:` (context | container | component)".into(), false);
                } else if !["context", "container", "component"].contains(&level.trim()) {
                    push(
                        Severity::Error,
                        "bad-level",
                        format!("`level: {}` is not one of context|container|component (V1's `project` level became the main.iter.md head; `code` is not implemented)", level.trim()),
                        false,
                    );
                }
                if !body.to_lowercase().contains("long description") {
                    push(Severity::Info, "no-long-description", "code node body has no `# Long Description` section (`iter stubdesc` can stub it; agents should write it for real)".into(), false);
                }
            }
        }
        Some(Role::Usecase) => {
            if has_frontmatter && front.children_present && front.child("codenodes").is_none() {
                push(
                    Severity::Error,
                    "missing-key",
                    "`children.codenodes:` is REQUIRED on a use-case (an empty list is valid — it marks agent work to come)".into(),
                    false,
                );
            }
            if !front.lists.get("participants").map(|l| l.is_empty()).unwrap_or(true) {
                push(
                    Severity::Warn,
                    "legacy-key",
                    "`participants:` is the V1 linking mechanism — V2 links a use-case to code via `children.codenodes` (the migration rewrites this)".into(),
                    false,
                );
            }
        }
        Some(Role::Interface) => {
            if has_frontmatter && !front.scalar("interface").trim().is_empty() {
                push(
                    Severity::Warn,
                    "legacy-key",
                    "`interface:` is the V1 id key — V2 uses `name:` (the engine reads it as a fallback; the migration rewrites it)".into(),
                    false,
                );
            }
            let kind = front.scalar("kind").trim().to_string();
            let kind_sections = interface_kind_sections(&kind);
            if kind.is_empty() {
                push(Severity::Warn, "missing-key", "interface file has no `kind:` (request-reply | event | stream | dataset)".into(), false);
            } else if kind_sections.is_none() {
                push(Severity::Warn, "bad-kind", format!("`kind: {}` is not a logical interaction kind (request-reply | event | stream | dataset) — transports and formats are not kinds", kind), false);
            }
            // The body IS the contract, in the fixed format (unchanged by V2):
            // an H1 title, a summary under 300 chars, the kind's required H2
            // sections, then `## Worked examples` (strict JSON) and
            // `## Invariants` last — optionally followed by `## Exceptions`.
            let contract = body.trim();
            if contract.is_empty() {
                push(
                    Severity::Warn,
                    "empty-contract-body",
                    "interface body is empty — the body IS the contract (get the skeleton: `iter validate --file <this file> --template`)".into(),
                    false,
                );
            } else {
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
                if h1_count == 0 {
                    push(Severity::Warn, "missing-summary", "no `# <id> — contract` H1 title; the fixed format is H1 + a summary under 300 characters, then the H2 sections".into(), false);
                } else if h1_count > 1 {
                    push(Severity::Warn, "multiple-h1", format!("{} H1 headings — the fixed format has exactly one, followed by the summary", h1_count), false);
                } else if summary_len == 0 {
                    push(Severity::Warn, "missing-summary", "no summary prose between the H1 title and the first H2 section".into(), false);
                } else if summary_len > 300 {
                    push(Severity::Warn, "summary-too-long", format!("the summary between the H1 and the first H2 is {} characters — the cap is 300; move detail into the sections", summary_len), false);
                }
                if let Some(required) = kind_sections {
                    let has = |name: &str| sections.iter().any(|s| s.eq_ignore_ascii_case(name));
                    for name in required.iter().chain(INTERFACE_TAIL_SECTIONS) {
                        if !has(name) {
                            push(Severity::Warn, "missing-section", format!("`kind: {}` requires a `## {}` section and the body has none", kind, name), false);
                        }
                    }
                    for s in &sections {
                        let allowed = required
                            .iter()
                            .chain(INTERFACE_TAIL_SECTIONS)
                            .chain(std::iter::once(&INTERFACE_OPTIONAL_TAIL_SECTION))
                            .any(|n| s.eq_ignore_ascii_case(n));
                        if !allowed {
                            push(Severity::Warn, "unexpected-section", format!("`## {}` is not a section of the fixed format for `kind: {}` — the contract holds ONLY the format's sections; other prose belongs on a code node or techreq", s, kind), false);
                        }
                    }
                    let exceptions_last = sections
                        .last()
                        .map(|s| s.eq_ignore_ascii_case(INTERFACE_OPTIONAL_TAIL_SECTION))
                        .unwrap_or(false);
                    if has(INTERFACE_OPTIONAL_TAIL_SECTION) && !exceptions_last {
                        push(Severity::Warn, "section-order", "`## Exceptions` is optional, but when it is present it must be the FINAL section, after `## Invariants`".into(), false);
                    }
                    let core = if exceptions_last { &sections[..sections.len() - 1] } else { &sections[..] };
                    let tail_ok = core.len() >= 2
                        && core[core.len() - 2].eq_ignore_ascii_case("Worked examples")
                        && core[core.len() - 1].eq_ignore_ascii_case("Invariants");
                    if !tail_ok && INTERFACE_TAIL_SECTIONS.iter().all(|n| has(n)) {
                        push(Severity::Warn, "section-order", "`## Worked examples` then `## Invariants` must be the last two sections, ahead of an optional closing `## Exceptions`".into(), false);
                    }
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
                            "usage details in the contract body (\"{}\") — an interface file records WHAT crosses the boundary, never who provides/consumes it or how; code nodes' inputs/outputs links carry that",
                            hits.join("\", \"")
                        ),
                        false,
                    );
                }
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

    // 5. Stray identity keys: harmless (the filename wins) but confusing.
    if has_frontmatter {
        let strays: &[&str] = match role {
            Some(Role::Code) => &["interface"],
            Some(Role::Interface) | Some(Role::Usecase) => &["level"],
            Some(Role::Main) => &[],
            _ => &["level", "interface"],
        };
        for key in strays {
            if front.scalars.contains_key(*key) {
                push(
                    Severity::Warn,
                    "stray-identity-key",
                    format!("`{}:` has no meaning in a {} file — the filename decides the role; remove the key or rename the file", key, markers::role_name(role)),
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
/// per nodetype, stubs in <angle brackets>. The role comes from the FILENAME;
/// interface skeletons follow the file's existing `kind:` when it declares a
/// valid one, and stub `request-reply` otherwise.
pub fn template_for(path: &Path) -> Result<String, String> {
    let fname = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let Some(role) = markers::role_of(&fname) else {
        return Err(format!(
            "`{}` matches no nodetype (`*.main|code|bizreq|techreq|interface|testgroup|usecase.iter.md`, the dot rule) — a plain context doc has no template",
            fname
        ));
    };
    Ok(match role {
        Role::Interface => {
            let declared = std::fs::read_to_string(path)
                .ok()
                .map(|t| markers::parse_front(&t).scalar("kind"))
                .unwrap_or_default();
            let kind = if interface_kind_sections(&declared).is_some() { declared } else { "request-reply".to_string() };
            interface_template(&kind)
        }
        Role::Main => MAIN_TEMPLATE.trim_start().to_string(),
        Role::Code => CODE_TEMPLATE.trim_start().to_string(),
        Role::Bizreq => req_template("bizreq", "business requirements", "WHAT the business needs — never how it is built"),
        Role::Techreq => req_template("techreq", "technical requirements", "a technical constraint the build must honor"),
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
        "---\nname: <kebab-case-id>\nkind: {}                    # request-reply | event | stream | dataset\ndescription: \"<one line: what data crosses this boundary>\"\nteststate: inherit\nowner: bespoke\nchildren:\n  bizreqs:    [\"{{thisfiledir}}/{{thisfilestem}}/*.bizreq.iter.md\"]\n  techreqs:   [\"{{thisfiledir}}/{{thisfilestem}}/*.techreq.iter.md\"]\n  testgroups: [\"{{thisfiledir}}/{{thisfilestem}}/*.testgroup.iter.md\"]\n---\n\n# <kebab-case-id> — contract\n\n<Named summary, under 300 characters: what goes in, what comes out, and why.\nNo carrier, no consumers, no deployment.>\n\n{}\n## Invariants\n\n- <property the examples cannot show: totality, determinism, ordering, limits,\n  closed vocabularies>\n- Transport-neutral: these messages ride any carrier unchanged; carrier\n  bindings (routes, ports, topics, flags, exit codes) live on the serving\n  node's code file, never here.\n\n## Exceptions\n\n<!-- none — a declared deviation from the internal transport law goes here (what deviates, why, what still holds); with no declaration there is no exception, so leave this section empty or drop it -->\n",
        kind, middle
    )
}

fn req_template(nodetype: &str, title: &str, statement: &str) -> String {
    format!(
        "---\nname: \"<Component> {title}\"\ndescription: \"<one line on what these requirements govern>\"\nchildren:\n  reqpaths: []          # body-only; point at external requirement docs here if any\n---\n\n# <component> — {title}\n\n- **<PREFIX>-{tag}-001** — <one requirement per bullet, stable id never renumbered,\n  a testable statement of {statement}.>\n",
        title = title,
        tag = nodetype.to_uppercase(),
        statement = statement
    )
}

const MAIN_TEMPLATE: &str = r#"
---
projectname: "<Project Name>"
projectdescription: "<one line on what this project is>"
globalscandirs: ["{topdir}/"]
globalinterfacedir: "{topdir}/interfaces/"
globalusecasedir: "{topdir}/usecases/"
globalcontextfiles: []
---

# <Project Name>

<The guiding high-level vision: what this project is, who it serves, and the
shape of the build. This body is the FIRST content loaded into every agent
context — keep it current.>
"#;

const CODE_TEMPLATE: &str = r#"
---
name: "<Human-Readable Name>"
level: component                # context | container | component
description: "<one line on what this code node is>"
owner: bespoke                  # bespoke | oss | 3rdparty
teststate: inherit              # omit | include | block | inherit
children:
  codedirs:   ["{thisfiledir}/"]
  codenodes:  []                # child *.code.iter.md files (paths or globs)
  inputs:     []                # interfaces consumed ({interfaces}/**/x.interface.iter.md)
  outputs:    []                # interfaces produced
  bizreqs:    ["{thisfiledir}/*.bizreq.iter.md"]
  techreqs:   ["{thisfiledir}/*.techreq.iter.md"]
  testgroups: ["{thisfiledir}/test/*.testgroup.iter.md"]
---

# Long Description

<Plain-language description for a non-technical reader — describe, don't
state; no jargon; define every acronym on first use; link related project
parts by their node file path. Never leave TBD.>

## <interface-id> binding

<Only if this node serves or consumes an interface over a specific carrier:
how the transport-neutral messages map to that carrier — routes, topics,
flags, exit codes. One short block per bound interface.>
"#;

const TESTGROUP_TEMPLATE: &str = r#"
---
name: "<Component> tests"
description: "<one line on what this group of tests proves>"
children:
  testpaths: ["{thisfiledir}/*.sh"]
---

# <component> — test groups

<!-- iterapp:testgroups
{"label":"<Group label>","testlist":[{"id":"t1","name":"<short name>","desc":"<what it proves>","shell":"t1.sh"}]}
-->
"#;

const USECASE_TEMPLATE: &str = r#"
---
name: "<Use-case name>"
description: "<one line on the thread>"
teststate: inherit
children:
  codenodes:  []                # REQUIRED key; the *.code.iter.md files this journey needs (may start empty)
  testgroups: ["{thisfiledir}/{thisfilestem}/*.testgroup.iter.md"]
---

# <Use-case name>

<The narrative: who initiates, what flows through which code nodes, what
comes out. Reference interfaces by id and code nodes by file path.>
"#;

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
            if matches!(name.as_str(), ".git" | "target" | "node_modules" | ".iter") {
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
    fn clean_code_node_passes_with_info_only() {
        let dir = tmp("clean");
        std::fs::create_dir_all(dir.join("test")).unwrap();
        std::fs::write(dir.join("test/comp.testgroup.iter.md"), "# tests\n").unwrap();
        let p = dir.join("comp.code.iter.md");
        std::fs::write(&p, "---\nname: \"Comp\"\nlevel: component\ndescription: \"a thing\"\nowner: bespoke\nchildren:\n  testgroups: [\"{thisfiledir}/test/*.testgroup.iter.md\"]\n---\n# Long Description\nreal words\n").unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(fs.iter().all(|f| f.severity == Severity::Info), "{:?}", fs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v1_filenames_are_called_out() {
        let dir = tmp("v1names");
        let m = dir.join("x.marker.iter.md");
        std::fs::write(&m, "---\nname: \"X\"\n---\nbody").unwrap();
        let fs = validate_file(&m, false).unwrap();
        assert!(codes(&fs).contains(&"v1-filename"), "{:?}", fs);
        assert!(fs.iter().any(|f| f.severity == Severity::Error && f.message.contains("code.iter.md")));

        let s = dir.join("my_thing_code.iter.md");
        std::fs::write(&s, "prose\n").unwrap();
        let fs = validate_file(&s, false).unwrap();
        assert!(codes(&fs).contains(&"v1-filename"), "missing-dot style flagged: {:?}", fs);

        let n = dir.join("notes.iter.md");
        std::fs::write(&n, "just notes\n").unwrap();
        let fs = validate_file(&n, false).unwrap();
        assert!(codes(&fs).contains(&"no-role"), "{:?}", fs);
        assert!(fs.iter().all(|f| f.severity == Severity::Info), "plain doc is info only: {:?}", fs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unquoted_prose_is_flagged_and_fixed() {
        let dir = tmp("quote");
        let p = dir.join("comp.code.iter.md");
        std::fs::write(&p, "---\nname: Comp\nlevel: component\ndescription: svc-intake: thin host for m08\nchildren:\n  bizreqs: []\n---\nbody\n").unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(codes(&fs).contains(&"unquoted-prose"), "{:?}", fs);
        assert!(!fs.iter().find(|f| f.code == "unquoted-prose").unwrap().fixed);

        let fs = validate_file(&p, true).unwrap();
        assert!(fs.iter().find(|f| f.code == "unquoted-prose").unwrap().fixed);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("description: \"svc-intake: thin host for m08\""), "{}", text);
        let fs = validate_file(&p, true).unwrap();
        assert!(!codes(&fs).contains(&"unquoted-prose"), "idempotent: {:?}", fs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dots_fence_and_leading_junk_are_fixed() {
        let dir = tmp("fence");
        let p = dir.join("x.code.iter.md");
        std::fs::write(&p, "\n\n---\nname: \"X\"\nlevel: component\ndescription: \"d\"\n...\nbody\n").unwrap();
        let fs = validate_file(&p, true).unwrap();
        let c = codes(&fs);
        assert!(c.contains(&"junk-before-fence") && c.contains(&"dots-fence"), "{:?}", fs);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.starts_with("---\n") && text.contains("\n---\nbody"), "{}", text);
        let front = markers::parse_front(&text);
        assert_eq!(front.scalar("name"), "X");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unterminated_frontmatter_is_an_error_not_a_guessed_fix() {
        let dir = tmp("unterminated");
        let p = dir.join("x.code.iter.md");
        let body = "---\nname: \"X\"\nlevel: component\nno closing fence anywhere\n";
        std::fs::write(&p, body).unwrap();
        let fs = validate_file(&p, true).unwrap();
        assert!(codes(&fs).contains(&"unterminated-frontmatter"), "{:?}", fs);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body, "no guessed rewrite");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn structure_v2_frontmatter_law() {
        let dir = tmp("law");
        // Missing children mapping.
        let p = dir.join("m.code.iter.md");
        std::fs::write(&p, "---\nname: \"M\"\nlevel: component\ndescription: \"d\"\n---\nbody").unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"missing-children"));
        // Unknown child sub-key.
        std::fs::write(&p, "---\nname: \"M\"\nlevel: component\ndescription: \"d\"\nchildren:\n  nonsense: []\n---\nbody").unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"unknown-child-key"));
        // Bad level (V1's project included).
        std::fs::write(&p, "---\nname: \"M\"\nlevel: project\ndescription: \"d\"\nchildren:\n  bizreqs: []\n---\nbody").unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"bad-level"));
        // Usecase without the required codenodes key.
        let u = dir.join("flow.usecase.iter.md");
        std::fs::write(&u, "---\nname: \"F\"\ndescription: \"d\"\nchildren:\n  testgroups: []\n---\nstory").unwrap();
        let fs = validate_file(&u, false).unwrap();
        assert!(fs.iter().any(|f| f.code == "missing-key" && f.message.contains("codenodes")), "{:?}", fs);
        // Empty codenodes is VALID.
        std::fs::write(&u, "---\nname: \"F\"\ndescription: \"d\"\nchildren:\n  codenodes: []\n---\nstory").unwrap();
        let fs = validate_file(&u, false).unwrap();
        assert!(!fs.iter().any(|f| f.message.contains("codenodes")), "{:?}", fs);
        // Legacy keys flagged.
        std::fs::write(&u, "---\nname: \"F\"\ndescription: \"d\"\ntest_loop: omit\nparticipants:\n  - 1 core\nchildren:\n  codenodes: []\n---\nstory").unwrap();
        let fs = validate_file(&u, false).unwrap();
        assert_eq!(fs.iter().filter(|f| f.code == "legacy-key").count(), 2, "{:?}", fs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fully format-compliant request-reply contract (V2 frontmatter).
    const CLEAN_IFACE: &str = "---\nname: pay-msg\nkind: request-reply\ndescription: \"a payment in, a receipt or refusal out\"\nchildren:\n  testgroups: []\n---\n\n# pay-msg — contract\n\nA payment request in; exactly one reply out — a receipt XOR a refusal.\n\n## Request\n\n```\n{ \"amount_cents\": 1200 }\n```\n\n## Reply, success shape\n\n```\n{ \"receipt_id\": \"r-1\" }\n```\n\n## Reply, failure shape\n\n```\n{ \"refusal\": { \"code\": \"NO_FUNDS\" } }\n```\n\n## Worked examples\n\n```json\n[ { \"request\": { \"amount_cents\": 1200 }, \"reply\": { \"receipt_id\": \"r-1\" } } ]\n```\n\n## Invariants\n\n- Deterministic and total.\n";

    #[test]
    fn interface_body_is_the_contract() {
        let dir = tmp("ifacebody");
        let p = dir.join("api.interface.iter.md");

        std::fs::write(&p, CLEAN_IFACE).unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(fs.is_empty(), "{:?}", fs);

        std::fs::write(&p, "---\nname: pay-msg\nkind: request-reply\ndescription: d\nchildren:\n  testgroups: []\n---\n").unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"empty-contract-body"));

        std::fs::write(&p, CLEAN_IFACE.replace("kind: request-reply", "kind: grpc")).unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"bad-kind"));

        // V1 `interface:` id key flagged as legacy.
        std::fs::write(&p, CLEAN_IFACE.replace("name: pay-msg", "interface: pay-msg")).unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(codes(&fs).contains(&"legacy-key"), "{:?}", fs);

        // Section order still enforced.
        let misplaced = CLEAN_IFACE.replacen(
            "## Worked examples",
            "## Exceptions\n\n- Redis protocol, not gRPC.\n\n## Worked examples",
            1,
        );
        std::fs::write(&p, misplaced).unwrap();
        assert!(codes(&validate_file(&p, false).unwrap()).contains(&"section-order"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn templates_match_the_validator() {
        let dir = tmp("templates");

        let p = dir.join("new.interface.iter.md");
        let t = template_for(&p).unwrap();
        for section in ["## Request", "## Reply, success shape", "## Reply, failure shape", "## Worked examples", "## Invariants", "## Exceptions"] {
            assert!(t.contains(section), "missing {} in:\n{}", section, t);
        }
        std::fs::write(&p, "---\nname: x\nkind: event\nchildren:\n  testgroups: []\n---\n").unwrap();
        let t = template_for(&p).unwrap();
        assert!(t.contains("## Event") && !t.contains("## Request"), "{}", t);

        // Every nodetype has a template carrying the children mapping; a
        // no-role filename is an error.
        for f in ["a.main.iter.md", "a.code.iter.md", "a.bizreq.iter.md", "a.techreq.iter.md", "a.testgroup.iter.md", "a.usecase.iter.md"] {
            let t = template_for(&dir.join(f)).unwrap();
            if !f.contains("main") {
                assert!(t.contains("children:"), "{} template lacks children:\n{}", f, t);
            }
        }
        assert!(template_for(&dir.join("notes.iter.md")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn testgroup_missing_script_is_an_error() {
        let dir = tmp("tgscripts");
        let p = dir.join("g.testgroup.iter.md");
        std::fs::write(&p, "---\nname: g\ndescription: d\nchildren:\n  testpaths: [\"{thisfiledir}/*.sh\"]\n---\n# t\n<!-- iterapp:testgroups\n{\"label\":\"G\",\"testlist\":[{\"id\":\"t1\",\"name\":\"t\",\"desc\":\"\",\"shell\":\"tests/iter/t1.sh\"}]}\n-->\n").unwrap();
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
        std::fs::write(dir.join("a.code.iter.md"), "---\nname: \"A\"\nlevel: component\ndescription: \"d\"\nchildren:\n  bizreqs: []\n---\n# Long Description\nx\n").unwrap();
        std::fs::write(dir.join("notes.iter.md"), "plain\n").unwrap();
        let report = run(&[dir.clone()], None, false).unwrap();
        assert_eq!(report.files_checked, 2);
        assert_eq!(report.worst(), Some(Severity::Info), "{:?}", report.findings);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
