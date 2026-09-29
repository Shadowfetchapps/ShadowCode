//! `docs/API_CONTRACT.md` lists every application route. This test reads
//! the route index there and the router's source, and fails when a route is
//! added, removed or given another method without the document following —
//! the same idea as the remote-access policy's "every family has a
//! decision" test.
//!
//! The source is read the way a reviewer reads it:
//! - every `"/api/…"` literal in a route module must be a documented route
//!   (a trailing `/` is a documented prefix, `{x}` a parameter), and a
//!   method literal on the same match arm must be documented with it;
//! - every string literal in a match-arm pattern that also names a method
//!   (`("POST", Some("pause")) =>`, `("GET", ["compare", id]) =>`) must be
//!   a segment of a documented route with that method in the module's
//!   families;
//! - every documented route's method and literal segments must still
//!   appear in the source of the modules serving its family.
//!
//! Routes matched without any literal (a new method on `/api/x/{id}` with
//! `("PUT", None)`) are beyond a text check; review them when adding one.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];

/// Each route module and the families (segment after `/api/`) it serves.
/// A new module under `service/` must be added here.
const MODULES: &[(&str, &[&str])] = &[
    ("service/about.rs", &["about", "updates"]),
    (
        "service/accounts.rs",
        &["accounts", "cli-agents", "openrouter", "allowance"],
    ),
    ("service/agents.rs", &["agents", "subagents"]),
    ("service/automations.rs", &["automations"]),
    ("service/background.rs", &["background"]),
    ("service/code_intel.rs", &["code-intel"]),
    ("service/commands.rs", &["commands", "memory"]),
    ("service/compare.rs", &["compare", "compares"]),
    ("service/composer.rs", &["workspace"]),
    ("service/data.rs", &["data"]),
    ("service/diagnostic_export.rs", &["diagnostic-exports"]),
    (
        "service/extensions.rs",
        &["plugins", "mcp", "hooks", "sqlite"],
    ),
    ("service/feed.rs", &["feed"]),
    ("service/forge.rs", &["git"]),
    ("service/git.rs", &["git"]),
    ("service/goals.rs", &["goals"]),
    ("service/inspection.rs", &["doctor"]),
    ("service/issues.rs", &["issues"]),
    (
        "service/jobs.rs",
        &["jobs", "run", "approvals", "checkpoints"],
    ),
    ("service/local_downloads.rs", &["local-models"]),
    ("service/memory.rs", &["memory"]),
    (
        "service/model_catalog.rs",
        &["providers", "models", "picker", "local-models"],
    ),
    ("service/preview.rs", &["preview"]),
    ("service/remote.rs", &["remote"]),
    ("service/review.rs", &["review"]),
    ("service/rules.rs", &["rules"]),
    ("service/sandbox.rs", &["sandbox"]),
    (
        "service/sessions.rs",
        &["sessions", "projects", "events", "resolve"],
    ),
    (
        "service/settings.rs",
        &[
            "config",
            "routing",
            "onboarding",
            "health",
            "version",
            "doctor",
            "diagnostic-exports",
            "guardian",
        ],
    ),
    ("service/terminals.rs", &["terminals"]),
    ("service/voice.rs", &["voice"]),
    ("service/workspace.rs", &["workspace"]),
    ("service/worktree_tasks.rs", &["worktree-tasks"]),
    ("service/worktrees.rs", &["worktrees", "parallel"]),
    ("service.rs", &["commands", "memory"]),
    // Served by the local control socket only (`shadowcode` CLI/TUI views).
    ("control.rs", &["runtime", "views", "owned-jobs"]),
];

/// Documented segments that are a fixed value of a parameter in the code,
/// with the source token that stands for them.
const LITERAL_PARAMETERS: &[(&str, &str)] = &[("antigravity", "Vendor::Antigravity")];

fn src() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
}

/// A route with its parameters normalized: `/api/sessions/{}/events`.
fn normalize(route: &str) -> String {
    route
        .split('/')
        .map(|part| {
            if part.starts_with('{') && part.ends_with('}') {
                "{}"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn family(route: &str) -> &str {
    route.split('/').nth(2).unwrap_or("")
}

/// Literal segments after the family.
fn segments(route: &str) -> Vec<&str> {
    route
        .split('/')
        .skip(3)
        .filter(|s| !s.is_empty() && !s.starts_with('{'))
        .collect()
}

/// `(method, route)` rows of the route index.
fn documented() -> Vec<(String, String)> {
    let doc = include_str!("../../../docs/API_CONTRACT.md");
    let block = doc
        .split_once("<!-- api-routes:begin -->")
        .expect("route index start marker")
        .1
        .split_once("<!-- api-routes:end -->")
        .expect("route index end marker")
        .0;
    let mut rows = Vec::new();
    for line in block.lines().filter(|l| l.starts_with("| `")) {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        let method = cells[1].trim_matches('`').to_owned();
        let route = cells[2].trim_matches('`').to_owned();
        assert!(METHODS.contains(&method.as_str()), "bad method: {line}");
        assert!(route.starts_with("/api/"), "bad route: {line}");
        assert!(
            !route.contains('?'),
            "no query strings in the index: {line}"
        );
        assert!(
            matches!(cells[3], "stable" | "experimental") || cells[3].starts_with("deprecated"),
            "stability must be stable, experimental or deprecated: {line}"
        );
        assert!(
            matches!(cells[4], "allowed" | "refused" | "switch" | "local-only"),
            "remote must be allowed, refused, switch or local-only: {line}"
        );
        rows.push((method, route));
    }
    rows
}

/// String literals of one line.
fn literals(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut chars = line.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut escaped = false;
        for (end, c) in chars.by_ref() {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => {
                    found.push(line[start + 1..end].to_owned());
                    break;
                }
                _ => escaped = false,
            }
        }
    }
    found
}

/// The module's code lines without unit tests and comments.
fn code_lines(file: &str) -> Vec<String> {
    let text = fs::read_to_string(src().join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
    let code = text.split("\n#[cfg(test)]").next().unwrap();
    code.lines()
        .filter(|line| {
            let line = line.trim_start();
            !line.starts_with("//") && !line.starts_with("///")
        })
        .map(|line| {
            // A trailing comment outside string literals.
            let mut quoted = false;
            let bytes = line.as_bytes();
            for i in 0..bytes.len() {
                if bytes[i] == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
                    quoted = !quoted;
                }
                if !quoted && bytes[i..].starts_with(b"//") {
                    return line[..i].to_owned();
                }
            }
            line.to_owned()
        })
        .collect()
}

#[test]
fn every_route_is_in_the_contract_and_every_documented_route_exists() {
    let rows = documented();
    assert!(rows.len() > 200, "parsed {} rows", rows.len());
    let normalized: BTreeSet<(String, String)> = rows
        .iter()
        .map(|(m, r)| (m.clone(), normalize(r)))
        .collect();
    assert_eq!(normalized.len(), rows.len(), "duplicate rows in the index");
    let any_method = |route: &str| normalized.iter().any(|(_, r)| r == route);

    // Every route module is known.
    let mut known: BTreeSet<&str> = MODULES.iter().map(|(file, _)| *file).collect();
    known.insert("service/call.rs");
    for entry in fs::read_dir(src().join("service")).unwrap().flatten() {
        let file = format!("service/{}", entry.file_name().to_string_lossy());
        assert!(
            known.contains(file.as_str()),
            "{file} is a new route module: add it and its families to MODULES in tests/api_contract.rs and document its routes in docs/API_CONTRACT.md"
        );
    }
    // Every family the dispatcher serves has a module here, and back.
    let service = fs::read_to_string(src().join("service.rs")).unwrap();
    let start = service.find("match call.family() {").unwrap();
    let end = start
        + service[start..]
            .find("_ => Err(call.unavailable()),\n        }\n    }")
            .unwrap();
    let mut dispatched = BTreeSet::new();
    for line in service[start..end].lines() {
        let head = line.split("=>").next().unwrap_or("");
        if head.contains('(') {
            continue;
        }
        dispatched.extend(literals(head));
    }
    let mapped: BTreeSet<String> = MODULES
        .iter()
        .filter(|(file, _)| *file != "control.rs")
        .flat_map(|(_, families)| families.iter().map(|f| f.to_string()))
        .collect();
    assert_eq!(dispatched, mapped, "dispatcher families vs MODULES");
    let documented_families: BTreeSet<String> =
        rows.iter().map(|(_, r)| family(r).to_owned()).collect();
    let all_families: BTreeSet<String> = MODULES
        .iter()
        .flat_map(|(_, families)| families.iter().map(|f| f.to_string()))
        .collect();
    assert_eq!(
        documented_families, all_families,
        "every family has documented routes, and no documented family is unserved"
    );

    let mut problems = Vec::new();
    let mut family_literals: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut family_text: BTreeMap<&str, String> = BTreeMap::new();
    for (file, families) in MODULES {
        let lines = code_lines(file);
        let module_routes: Vec<&(String, String)> = rows
            .iter()
            .filter(|(_, r)| families.contains(&family(r)))
            .collect();
        for line in &lines {
            let found = literals(line);
            for family in *families {
                family_literals
                    .entry(family)
                    .or_default()
                    .extend(found.iter().cloned());
                let text = family_text.entry(family).or_default();
                text.push_str(line);
                text.push('\n');
            }
            let arm = line.contains("=>") || line.contains("==");
            let methods: Vec<&String> = found
                .iter()
                .filter(|l| METHODS.contains(&l.as_str()))
                .collect();
            // Full paths and prefixes.
            for path in found.iter().filter(|l| l.starts_with("/api/")) {
                let path = normalize(path.split('?').next().unwrap());
                if path.ends_with('/') {
                    if !normalized.iter().any(|(_, r)| r.starts_with(&path)) {
                        problems.push(format!("{file}: routes under {path} are not documented"));
                    }
                } else if !any_method(&path) {
                    problems.push(format!("{file}: {path} is not documented"));
                } else if arm {
                    for method in &methods {
                        if !normalized.contains(&(method.to_string(), path.clone())) {
                            problems.push(format!("{file}: {method} {path} is not documented"));
                        }
                    }
                }
            }
            // Segments named in a route arm next to its method.
            let Some((pattern, _)) = line.split_once("=>") else {
                continue;
            };
            let pattern = literals(pattern);
            let arm_methods: Vec<&String> = pattern
                .iter()
                .filter(|l| METHODS.contains(&l.as_str()))
                .collect();
            for segment in pattern.iter().filter(|l| {
                !METHODS.contains(&l.as_str())
                    && !l.starts_with("/api/")
                    && l.as_str() != "api"
                    && !families.contains(&l.as_str())
            }) {
                for method in &arm_methods {
                    if !module_routes
                        .iter()
                        .any(|(m, r)| m == *method && r.split('/').any(|s| s == segment))
                    {
                        problems.push(format!(
                            "{file}: {method} …/{segment} is not documented ({})",
                            line.trim()
                        ));
                    }
                }
            }
        }
    }
    // Documented routes still exist.
    for (method, route) in &rows {
        let family = family(route);
        let found = family_literals.get(family).cloned().unwrap_or_default();
        let text = family_text.get(family).cloned().unwrap_or_default();
        if !found.contains(method) {
            problems.push(format!(
                "{method} {route}: no {method} route left in the {family} modules"
            ));
        }
        if found.iter().any(|l| normalize(l) == normalize(route)) {
            continue;
        }
        for segment in segments(route) {
            let in_path = found
                .iter()
                .filter(|l| l.starts_with("/api/"))
                .any(|l| l.split('/').any(|s| s == segment));
            let literal_parameter = LITERAL_PARAMETERS
                .iter()
                .any(|(value, token)| *value == segment && text.contains(token));
            if !found.contains(segment) && !in_path && !literal_parameter {
                problems.push(format!(
                    "{method} {route}: \"{segment}\" no longer appears in the {family} modules"
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "docs/API_CONTRACT.md and the router disagree; update the route index (and its section) or the code:\n{}",
        problems.join("\n")
    );
}

#[test]
fn the_contract_has_a_versioning_policy_and_old_links_point_to_it() {
    let doc = include_str!("../../../docs/API_CONTRACT.md");
    for heading in [
        "## Versioning policy",
        "## Conventions",
        "## Errors",
        "## Events",
        "## Route index",
        "## Profile folders",
    ] {
        assert!(doc.contains(heading), "missing {heading}");
    }
    let old = include_str!("../../../docs/API_CONTRACT_0.28.md");
    assert!(
        old.contains("API_CONTRACT.md"),
        "the 0.28 file points to the current contract"
    );
    // Every section a row links to exists.
    let anchors: BTreeSet<String> = doc
        .lines()
        .filter_map(|l| l.strip_prefix("## "))
        .map(|heading| {
            heading
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
                .collect::<String>()
                .replace(' ', "-")
        })
        .collect();
    let block = doc
        .split_once("<!-- api-routes:begin -->")
        .unwrap()
        .1
        .split_once("<!-- api-routes:end -->")
        .unwrap()
        .0;
    for line in block.lines().filter(|l| l.starts_with("| `")) {
        let anchor = line
            .split("](#")
            .nth(1)
            .and_then(|rest| rest.split(')').next())
            .unwrap_or_else(|| panic!("row without a section link: {line}"));
        assert!(anchors.contains(anchor), "no section #{anchor}: {line}");
    }
}
