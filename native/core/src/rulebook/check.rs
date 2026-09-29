//! The skill checker: reads profile and project rules, skills, commands and
//! agent definitions and reports problems. It only reports; it never edits
//! a file.
//!
//! Checks: front matter that does not parse or breaks a limit; a skill
//! folder without `SKILL.md`; files a skill links to that do not exist;
//! names used twice (and which one wins); descriptions too long for the
//! skill list small local models read; fields ShadowCode ignores (such as
//! `allowed-tools`, which would otherwise pre-approve tools); text that asks
//! an agent to switch off approvals or the sandbox, pipe downloads into a
//! shell, or read credentials; rules files over their size limit; and a
//! skill list that no longer fits its budget.
use super::{Book, PROFILE_FILE_BYTES, SKILL_INDEX_BYTES, SKILL_INDEX_ENTRIES};
use crate::{
    instructions,
    workflows::{self, Catalog, Definition},
    workspace::Workspace,
};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, sync::OnceLock};

/// A description longer than this crowds the skill list small local
/// models see (ShadowCode shows at most 240 bytes of it).
pub const SHORT_DESCRIPTION: usize = 200;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Finding {
    /// `error`, `warning` or `info`.
    pub severity: String,
    pub code: String,
    /// `profile` or `project`.
    pub scope: String,
    pub path: String,
    pub name: String,
    pub message: String,
    pub fix: String,
}

fn finding(
    severity: &str,
    code: &str,
    scope: &str,
    path: &str,
    name: &str,
    message: String,
    fix: &str,
) -> Finding {
    Finding {
        severity: severity.into(),
        code: code.into(),
        scope: scope.into(),
        path: path.into(),
        name: name.into(),
        message,
        fix: fix.into(),
    }
}

struct Pattern {
    re: Regex,
    what: &'static str,
}

/// Text that asks an agent to act unsafely. Matching is a heuristic: it
/// flags wording for a person to review and blocks nothing.
fn patterns() -> &'static [Pattern] {
    static PATTERNS: OnceLock<Vec<Pattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            (r"(?i)--dangerously-skip-permissions|--allow-dangerously-skip-permissions|bypasspermissions|--yolo\b|danger-full-access|--full-auto\b", "turns off an agent's approvals or sandbox"),
            (r"(?i)approval[_ -]?policy\W{0,4}never|ask-for-approval\W{0,4}never", "turns off approvals"),
            (r"(?i)\b(disable|turn off|switch off|skip|bypass)\s+(the\s+|all\s+|any\s+)?(approvals?|approval prompts?|permission (prompts?|checks?)|permissions|sandbox(ing)?|confirmations?)", "asks to switch off approvals or the sandbox"),
            (r"(?i)\b(without|never)\s+(asking|ask(ing)? for)\s+(for\s+)?(approval|permission|confirmation)", "asks to act without approval"),
            (r"(?i)\bauto-?approve\b", "asks to approve actions automatically"),
            (r"(?i)ignore\s+(all\s+|any\s+)?(previous|prior|earlier|above)\s+instructions", "tries to override earlier instructions"),
            (r"(?i)\b(curl|wget)\b[^|\n]{0,200}\|\s*(sudo\s+)?(ba|z|da)?sh\b", "pipes a download straight into a shell"),
            (r"(?i)\brm\s+-(rf|fr)\s+(/|~|\$home)(\s|$)", "deletes a home or root folder"),
            (r"(?i)\bchmod\s+(-r\s+)?777\b", "makes files writable by everyone"),
            (r"(?i)(\.ssh/|id_rsa|id_ed25519|\.aws/credentials|\.netrc|secrets\.env|\.git-credentials)", "mentions a credential file"),
            (r"(?i)\bexfiltrat", "mentions sending data out"),
        ]
        .into_iter()
        .map(|(re, what)| Pattern {
            re: Regex::new(re).expect("checker pattern"),
            what,
        })
        .collect()
    })
}

/// Unsafe wording in `text`: `(what, the matched words)`, first match of
/// each kind, at most eight.
pub fn unsafe_content(text: &str) -> Vec<(&'static str, String)> {
    let mut found = Vec::new();
    for pattern in patterns() {
        if let Some(m) = pattern.re.find(text) {
            let words: String = m.as_str().chars().take(80).collect();
            found.push((pattern.what, words));
        }
        if found.len() >= 8 {
            break;
        }
    }
    found
}

/// Relative files a skill links to (`[text](scripts/run.sh)`).
fn linked_files(body: &str) -> Vec<String> {
    static LINK: OnceLock<Regex> = OnceLock::new();
    let link = LINK.get_or_init(|| Regex::new(r"\]\(([^)\s]+)\)").expect("link pattern"));
    let mut out = Vec::new();
    for capture in link.captures_iter(body).take(64) {
        let target = capture[1].split('#').next().unwrap_or("").trim();
        if target.is_empty()
            || target.contains("://")
            || target.starts_with("mailto:")
            || target.starts_with('/')
            || target.starts_with('~')
            || target.starts_with('<')
        {
            continue;
        }
        if !out.iter().any(|t| t == target) {
            out.push(target.to_owned());
        }
    }
    out
}

/// One definition file to check.
struct File<'a> {
    scope: &'static str,
    /// Relative to `root`.
    rel: String,
    /// What the user sees.
    shown: String,
    kind: &'static str,
    root: &'a Workspace,
    compat: bool,
    source: String,
}

fn check_file(file: &File, findings: &mut Vec<Finding>) -> Option<Definition> {
    let fix_front = "Fix the front matter between the --- lines (YAML with name and description).";
    let text = match file.root.read(&file.rel) {
        Ok(read) => read,
        Err(error) => {
            let missing = error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                || error.to_string().contains("File not found");
            findings.push(if missing && file.rel.ends_with("/SKILL.md") {
                finding(
                    "error",
                    "missing-file",
                    file.scope,
                    &file.shown,
                    "",
                    "This skill folder has no SKILL.md, so the skill cannot be used".into(),
                    "Add SKILL.md to the folder, or remove the folder.",
                )
            } else {
                finding(
                    "error",
                    "unreadable",
                    file.scope,
                    &file.shown,
                    "",
                    format!("Cannot read this file: {error:#}"),
                    "Make it a UTF-8 text file inside the folder (links that point outside are not followed).",
                )
            });
            return None;
        }
    };
    let definition = match Definition::parse_from(
        &file.shown,
        file.kind,
        &text.content,
        &text.hash,
        &file.source,
        file.compat,
    ) {
        Ok(definition) => definition,
        Err(error) => {
            let message = format!("{error:#}");
            let code = if message.contains("exceeds") {
                "too-large"
            } else {
                "front-matter"
            };
            findings.push(finding(
                "error",
                code,
                file.scope,
                &file.shown,
                "",
                message,
                fix_front,
            ));
            return None;
        }
    };
    let name = definition.info.name.clone();
    // Front matter the definition carries but ShadowCode never applies.
    if let Some(header) = text
        .content
        .replace("\r\n", "\n")
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---"))
        .map(|(header, _)| header.to_owned())
    {
        if let Ok(serde_json::Value::Object(meta)) = serde_yaml_ng::from_str::<Value>(&header) {
            for field in ["allowed-tools", "permission-mode", "hooks", "model"] {
                if meta.contains_key(field) {
                    findings.push(finding(
                        "warning",
                        "ignored-field",
                        file.scope,
                        &file.shown,
                        &name,
                        format!("`{field}` is ignored: skills shape how an agent works but never grant tools, permissions or models"),
                        "Remove the field, or keep it for other agents knowing ShadowCode will not apply it.",
                    ));
                }
            }
        }
    }
    if file.kind == "skill" {
        let undescribed = !text.content.contains("\ndescription:");
        if undescribed && definition.model_invocable {
            findings.push(finding(
                "warning",
                "no-description",
                file.scope,
                &file.shown,
                &name,
                "No description: agents pick skills by their description, so this one is unlikely to be used on its own".into(),
                "Add a one-line description: what the skill does and when to use it.",
            ));
        } else if definition.description.len() > SHORT_DESCRIPTION {
            findings.push(finding(
                "warning",
                "long-description",
                file.scope,
                &file.shown,
                &name,
                format!(
                    "The description is {} bytes; small local models see a list of every skill, so keep it under {SHORT_DESCRIPTION}",
                    definition.description.len()
                ),
                "Shorten the description to one sentence; move detail into the body.",
            ));
        }
    }
    for (what, words) in unsafe_content(&definition.content) {
        findings.push(finding(
            "warning",
            "unsafe-content",
            file.scope,
            &file.shown,
            &name,
            format!("This text {what}: \"{words}\". Agents still ask before acting, but review why it is here"),
            "Remove or reword it unless you are sure it is safe.",
        ));
    }
    // Files the skill links to, relative to its folder.
    if file.rel.ends_with("/SKILL.md") {
        let dir = Path::new(&file.rel).parent().unwrap_or(Path::new(""));
        for target in linked_files(&definition.content) {
            let path = dir.join(&target);
            let exists = file
                .root
                .snapshot(&path.to_string_lossy())
                .is_ok_and(|s| s.bytes.is_some())
                || file.root.list(&path.to_string_lossy()).is_ok();
            if !exists {
                findings.push(finding(
                    "warning",
                    "missing-file",
                    file.scope,
                    &file.shown,
                    &name,
                    format!("Links to {target}, which is not in the skill folder"),
                    "Add the file or fix the link.",
                ));
            }
        }
    }
    Some(definition)
}

fn check_rules(
    scope: &'static str,
    path: &str,
    content: &str,
    limit: usize,
    findings: &mut Vec<Finding>,
) {
    if content.len() > limit {
        findings.push(finding(
            "warning",
            "too-large",
            scope,
            path,
            "",
            format!(
                "{} bytes: agents read only the first {limit}",
                content.len()
            ),
            "Move detail into skills, which agents load only when they need them.",
        ));
    }
    for (what, words) in unsafe_content(content) {
        findings.push(finding(
            "warning",
            "unsafe-content",
            scope,
            path,
            "",
            format!("This text {what}: \"{words}\". Agents still ask before acting, but review why it is here"),
            "Remove or reword it unless you are sure it is safe.",
        ));
    }
}

/// Check the profile and, when given, the project. `GET /api/rules/check`
/// and `shadowcode rules check`.
pub fn run(book: &Book, workspace: Option<&Workspace>) -> Value {
    let mut findings = Vec::new();
    let mut checked = 0usize;
    let mut definitions: Vec<(Definition, &'static str)> = Vec::new();
    for issue in &book.issues {
        findings.push(finding(
            "error",
            "profile",
            "profile",
            &book.dir.display().to_string(),
            "",
            issue.clone(),
            "",
        ));
    }
    // Profile: rules files, then each source's skills and commands.
    for rules in book.profile_rules() {
        checked += 1;
        check_rules(
            "profile",
            &rules.path,
            &rules.content,
            PROFILE_FILE_BYTES,
            &mut findings,
        );
    }
    if let Some(profile) = book.profile.as_ref() {
        for (source, prefix) in book.sources() {
            let mut scratch = Catalog::default();
            let roots: Vec<_> = workflows::PROFILE_ROOTS
                .iter()
                .map(|(root, kind)| (format!("{prefix}{root}"), *kind))
                .collect();
            for (rel, kind) in workflows::scan(profile, &roots, &mut scratch) {
                checked += 1;
                let file = File {
                    scope: "profile",
                    shown: book.path_of(&rel),
                    rel: rel.clone(),
                    kind,
                    root: profile,
                    compat: true,
                    source: source.clone(),
                };
                if let Some(definition) = check_file(&file, &mut findings) {
                    definitions.push((definition, "profile"));
                }
            }
            for issue in scratch.issues {
                findings.push(finding(
                    "warning",
                    "limit",
                    "profile",
                    &book.dir.display().to_string(),
                    "",
                    issue,
                    "",
                ));
            }
        }
        for (dir, _) in book.agent_dirs() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten().take(128) {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "md") {
                    continue;
                }
                checked += 1;
                let shown = path.display().to_string();
                let parsed = std::fs::read_to_string(&path)
                    .map_err(anyhow::Error::from)
                    .and_then(|text| {
                        crate::agents::parse(
                            &shown,
                            &text,
                            &crate::workspace::hash(text.as_bytes()),
                            "profile",
                        )
                    });
                match parsed {
                    Ok(agent) => {
                        for (what, words) in unsafe_content(&agent.instructions) {
                            findings.push(finding(
                                "warning",
                                "unsafe-content",
                                "profile",
                                &shown,
                                &agent.name,
                                format!("This text {what}: \"{words}\". Agents still ask before acting, but review why it is here"),
                                "Remove or reword it unless you are sure it is safe.",
                            ));
                        }
                    }
                    Err(error) => findings.push(finding(
                        "error",
                        "front-matter",
                        "profile",
                        &shown,
                        "",
                        format!("{error:#}"),
                        "Fix the agent definition's front matter (name, description, tools, mode).",
                    )),
                }
            }
        }
    }
    // Project: root rules and definitions.
    if let Some(workspace) = workspace {
        let (files, _) = instructions::root_files(workspace);
        for file in files {
            checked += 1;
            check_rules(
                "project",
                &file.path,
                &file.content,
                instructions::ROOT_FILE_BYTES,
                &mut findings,
            );
        }
        let mut scratch = Catalog::default();
        let roots: Vec<_> = workflows::PROJECT_ROOTS
            .iter()
            .map(|(root, kind)| ((*root).to_owned(), *kind))
            .collect();
        for (rel, kind) in workflows::scan(workspace, &roots, &mut scratch) {
            checked += 1;
            let file = File {
                scope: "project",
                shown: rel.clone(),
                rel: rel.clone(),
                kind,
                root: workspace,
                compat: rel.starts_with(".claude/"),
                source: "project".into(),
            };
            if let Some(definition) = check_file(&file, &mut findings) {
                definitions.push((definition, "project"));
            }
        }
    }
    // Names used more than once, and which one is used.
    // (kind, name) → [(path, scope)]
    type Uses = Vec<(String, &'static str)>;
    let mut by_name: BTreeMap<(String, String), Uses> = BTreeMap::new();
    for (definition, scope) in &definitions {
        by_name
            .entry((definition.info.kind.clone(), definition.info.name.clone()))
            .or_default()
            .push((definition.info.path.clone(), scope));
    }
    for ((kind, name), uses) in &by_name {
        if uses.len() < 2 {
            continue;
        }
        let project: Vec<_> = uses.iter().filter(|u| u.1 == "project").collect();
        let profile: Vec<_> = uses.iter().filter(|u| u.1 == "profile").collect();
        if project.len() > 1 {
            let native = project
                .iter()
                .filter(|u| !u.0.starts_with(".claude/"))
                .count();
            if native != 1 {
                findings.push(finding(
                    "error",
                    "duplicate-name",
                    "project",
                    &project[0].0,
                    name,
                    format!(
                        "The project defines the {kind} /{name} more than once ({}); it cannot be used until the names differ",
                        project.iter().map(|u| u.0.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                    "Rename one of them (name or folder), or remove the copy.",
                ));
            }
        }
        if !project.is_empty() && !profile.is_empty() {
            findings.push(finding(
                "info",
                "duplicate-name",
                "profile",
                &profile[0].0,
                name,
                format!(
                    "Your profile and this project both define the {kind} /{name}; the project's ({}) is used here",
                    project[0].0
                ),
                "Rename your profile copy if you want both.",
            ));
        } else if profile.len() > 1 {
            findings.push(finding(
                "warning",
                "duplicate-name",
                "profile",
                &profile[0].0,
                name,
                format!(
                    "Your profile defines the {kind} /{name} more than once ({}); your own file wins over imports",
                    profile.iter().map(|u| u.0.as_str()).collect::<Vec<_>>().join(", ")
                ),
                "Rename one of them, or switch one off in Rules & skills.",
            ));
        }
    }
    // The skill list small models read.
    if let Some(workspace) = workspace {
        let skills: Vec<Definition> = book
            .catalog(workspace)
            .definitions
            .into_iter()
            .filter(|d| d.info.kind == "skill" && d.model_invocable)
            .collect();
        let bytes: usize = skills
            .iter()
            .map(|d| d.info.name.len() + d.description.len().min(240) + 5)
            .sum();
        if skills.len() > SKILL_INDEX_ENTRIES || bytes > SKILL_INDEX_BYTES {
            findings.push(finding(
                "info",
                "index-full",
                "project",
                "",
                "",
                format!(
                    "{} skills with {bytes} bytes of names and descriptions: agents are shown at most {SKILL_INDEX_ENTRIES} skills and {SKILL_INDEX_BYTES} bytes, so some are left out of the list",
                    skills.len()
                ),
                "Switch off skills you do not need here, or shorten descriptions.",
            ));
        }
    }
    let count = |severity: &str| findings.iter().filter(|f| f.severity == severity).count();
    let (errors, warnings, infos) = (count("error"), count("warning"), count("info"));
    json!({
        "ok": errors == 0,
        "checked": checked,
        "errors": errors,
        "warnings": warnings,
        "infos": infos,
        "findings": findings,
        "profile": book.dir,
        "workspace": workspace.map(|w| w.path.clone()),
        "note": "The checker only reports. It never changes a file.",
    })
}

/// Plain text for `shadowcode rules check`.
pub fn text(report: &Value) -> String {
    let mut out = String::new();
    let findings = report["findings"].as_array().cloned().unwrap_or_default();
    if findings.is_empty() {
        out.push_str(&format!(
            "No problems found in {} rules, skill and command files.\n",
            report["checked"]
        ));
    }
    for f in &findings {
        let severity = match f["severity"].as_str() {
            Some("error") => "Error",
            Some("warning") => "Warning",
            _ => "Note",
        };
        let path = f["path"].as_str().unwrap_or("");
        out.push_str(&format!(
            "{severity}: {}{}\n",
            if path.is_empty() {
                String::new()
            } else {
                format!("{path}: ")
            },
            f["message"].as_str().unwrap_or("")
        ));
        if let Some(fix) = f["fix"].as_str().filter(|s| !s.is_empty()) {
            out.push_str(&format!("  {fix}\n"));
        }
    }
    out.push_str(&format!(
        "{} checked · {} errors · {} warnings · {} notes. Nothing was changed.\n",
        report["checked"], report["errors"], report["warnings"], report["infos"]
    ));
    out
}
