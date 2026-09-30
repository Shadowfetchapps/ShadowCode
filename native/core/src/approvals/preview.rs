//! What an approval prompt shows before the user answers: the diff of a file
//! change against the file as it is now (the whole content for a new file),
//! or the full command with the folder it runs in.
//!
//! Previews are computed without touching the project. A preview that cannot
//! be computed (the file is binary, too large or already differs from what
//! the tool expects) is simply absent; the tool reports its own error.
//!
//! Shapes (all JSON):
//! - `{kind:"files", files:[{path, status, diff, added, removed, truncated, binary}]}`
//!   where `status` is `added`, `modified` or `deleted` and `diff` is a
//!   unified diff body (`@@` headers and `+`/`-`/space lines);
//! - `{kind:"command", command, cwd}`;
//! - `{kind:"move", from, to}` and `{kind:"folder", path}`.
use crate::{textdiff, workspace::Workspace};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Diff lines kept per file; the card shows the first few and "Show all".
pub const MAX_LINES: usize = 2000;
/// Files larger than this are not diffed.
const MAX_BYTES: usize = 2_000_000;

/// One file's change. `None` sides are a missing file.
pub fn file_change(path: &str, before: Option<&[u8]>, after: Option<&[u8]>) -> Value {
    let status = match (before, after) {
        (None, Some(_)) => "added",
        (Some(_), None) => "deleted",
        _ => "modified",
    };
    let text = |bytes: Option<&[u8]>| -> Option<Option<String>> {
        match bytes {
            None => Some(Some(String::new())),
            Some(b) if b.len() > MAX_BYTES || b.contains(&0) => None,
            Some(b) => std::str::from_utf8(b).ok().map(|s| Some(s.to_owned())),
        }
    };
    let (Some(Some(old)), Some(Some(new))) = (text(before), text(after)) else {
        return json!({"path":path,"status":status,"binary":true,"diff":"","added":0,"removed":0,"truncated":false});
    };
    let (diff, truncated, added, removed) = textdiff::unified(&old, &new, MAX_LINES);
    json!({
        "path": path,
        "status": status,
        "diff": diff,
        "added": added,
        "removed": removed,
        "truncated": truncated,
        "binary": false,
    })
}

/// Diff text kept across all files of one prompt: the preview is stored
/// with the `approval.requested` event. Files past it keep their counts.
const MAX_TOTAL_DIFF: usize = 256 * 1024;

fn files(mut changes: Vec<Value>) -> Value {
    if changes.is_empty() {
        return Value::Null;
    }
    let mut budget = MAX_TOTAL_DIFF;
    for change in &mut changes {
        let size = change["diff"].as_str().map_or(0, str::len);
        if size > budget {
            change["diff"] = json!("");
            change["truncated"] = json!(true);
            budget = 0;
        } else {
            budget -= size;
        }
    }
    json!({"kind":"files","files":changes})
}

pub fn command(command: &str, cwd: &Path) -> Value {
    json!({"kind":"command","command":command,"cwd":cwd})
}

/// The folder a command runs in: `cwd` inside the project, else the project.
fn folder(root: &Path, cwd: Option<&str>) -> PathBuf {
    match cwd.filter(|c| !c.trim().is_empty()) {
        Some(dir) if Path::new(dir).is_absolute() => PathBuf::from(dir),
        Some(dir) => root.join(dir),
        None => root.to_owned(),
    }
}

fn command_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

/// A file's path, its content before and after a change (`None`: missing).
pub type FileContents = (String, Option<Vec<u8>>, Option<Vec<u8>>);

/// The files a native file tool would change: `(path, before, after)`, with
/// `None` for a missing file. Empty for other tools or when the change
/// cannot be worked out ahead.
pub fn native_contents(workspace: &Workspace, tool: &str, args: &Value) -> Vec<FileContents> {
    let path = || args["path"].as_str();
    let before = |path: &str| workspace.snapshot(path).ok();
    let contents = || -> Option<Vec<FileContents>> {
        Some(match tool {
            "write_file" => {
                let path = path()?;
                vec![(
                    path.to_owned(),
                    before(path)?.bytes,
                    Some(args["content"].as_str()?.as_bytes().to_vec()),
                )]
            }
            "edit_file" => {
                let path = path()?;
                let current = before(path)?.bytes?;
                let text = std::str::from_utf8(&current).ok()?;
                let updated = if let Some(old) = args["old_string"].as_str() {
                    let new = args["new_string"].as_str()?;
                    // Matched exactly as the edit will run, close matches too.
                    crate::tools::replace_string(text, old, new, args["replace_all"] == true)
                        .ok()?
                        .0
                } else {
                    crate::tools::edit_line_hunks(text, args["hunks"].as_array()?).ok()?
                };
                vec![(
                    path.to_owned(),
                    Some(current.clone()),
                    Some(updated.into_bytes()),
                )]
            }
            "apply_patch" => {
                let patch = args["patch"].as_str().or_else(|| args["diff"].as_str())?;
                let patch = if patch.starts_with("@@") {
                    format!("--- a/{0}\n+++ b/{0}\n{patch}", path()?)
                } else {
                    patch.to_owned()
                };
                crate::patch::prepare(workspace, &patch)
                    .ok()?
                    .into_iter()
                    .map(|c| (c.path.clone(), c.before.bytes.clone(), c.after.clone()))
                    .collect()
            }
            _ => return None,
        })
    };
    contents().unwrap_or_default()
}

/// Preview of a native tool call.
pub fn native(workspace: &Workspace, tool: &str, args: &Value) -> Value {
    native_changes(workspace, tool, args).unwrap_or(Value::Null)
}

fn native_changes(workspace: &Workspace, tool: &str, args: &Value) -> Option<Value> {
    let path = || args["path"].as_str();
    let before = |path: &str| workspace.snapshot(path).ok();
    Some(match tool {
        "exec" | "background_start" => command(
            &command_text(&args["command"])?,
            &folder(&workspace.path, args["cwd"].as_str()),
        ),
        "write_file" => {
            let path = path()?;
            let current = before(path)?;
            files(vec![file_change(
                path,
                current.bytes.as_deref(),
                Some(args["content"].as_str()?.as_bytes()),
            )])
        }
        "edit_file" => {
            let path = path()?;
            let current = before(path)?;
            let bytes = current.bytes.as_deref()?;
            let text = std::str::from_utf8(bytes).ok()?;
            let updated = if let Some(old) = args["old_string"].as_str() {
                let new = args["new_string"].as_str()?;
                crate::tools::replace_string(text, old, new, args["replace_all"] == true)
                    .ok()?
                    .0
            } else {
                crate::tools::edit_line_hunks(text, args["hunks"].as_array()?).ok()?
            };
            files(vec![file_change(
                path,
                Some(bytes),
                Some(updated.as_bytes()),
            )])
        }
        "apply_patch" => {
            let patch = args["patch"].as_str().or_else(|| args["diff"].as_str())?;
            let patch = if patch.starts_with("@@") {
                format!("--- a/{0}\n+++ b/{0}\n{patch}", path()?)
            } else {
                patch.to_owned()
            };
            let changes = crate::patch::prepare(workspace, &patch).ok()?;
            files(
                changes
                    .iter()
                    .map(|c| file_change(&c.path, c.before.bytes.as_deref(), c.after.as_deref()))
                    .collect(),
            )
        }
        "delete_file" => {
            let path = path()?;
            let current = before(path)?;
            files(vec![file_change(path, current.bytes.as_deref(), None)])
        }
        "move_file" => json!({"kind":"move","from":args["src"],"to":args["dest"]}),
        "create_directory" => json!({"kind":"folder","path":path()?}),
        _ => return None,
    })
}

/// Preview of a vendor CLI's permission prompt (`ApprovalPrompt::arguments`
/// as the adapters write them), for the shapes the protocols describe.
pub fn vendor(root: &Path, kind: &str, args: &Value) -> Value {
    vendor_changes(root, kind, args).unwrap_or(Value::Null)
}

fn vendor_changes(root: &Path, kind: &str, args: &Value) -> Option<Value> {
    let input = &args["input"];
    if kind == "command" {
        let text = command_text(&args["command"]).or_else(|| command_text(&input["command"]))?;
        let cwd = args["cwd"].as_str().or_else(|| input["cwd"].as_str());
        return Some(command(&text, &folder(root, cwd)));
    }
    if kind != "file_change" {
        return None;
    }
    let workspace = Workspace::open(root).ok()?;
    let current = |path: &str| -> Option<Option<Vec<u8>>> {
        let relative = workspace.relative(path).ok()?;
        Some(workspace.snapshot(&relative.to_string_lossy()).ok()?.bytes)
    };
    let shown = |path: &str| {
        workspace
            .relative(path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_owned())
    };
    // Claude Code: Write / Edit / MultiEdit inputs.
    if let Some(path) = input["file_path"].as_str() {
        let before = current(path)?;
        let after = match args["tool"].as_str().unwrap_or("") {
            "Write" => input["content"].as_str()?.to_owned(),
            "Edit" | "MultiEdit" => {
                let mut text = String::from_utf8(before.clone()?).ok()?;
                let edits = match input["edits"].as_array() {
                    Some(list) => list.clone(),
                    None => vec![input.clone()],
                };
                for edit in edits {
                    let old = edit["old_string"].as_str()?;
                    let new = edit["new_string"].as_str()?;
                    if old.is_empty() || !text.contains(old) {
                        return None;
                    }
                    text = if edit["replace_all"] == true {
                        text.replace(old, new)
                    } else {
                        text.replacen(old, new, 1)
                    };
                }
                text
            }
            _ => return None,
        };
        return Some(files(vec![file_change(
            &shown(path),
            before.as_deref(),
            Some(after.as_bytes()),
        )]));
    }
    // ACP: `diff` content blocks on the tool call.
    if let Some(blocks) = args["content"].as_array() {
        let changes: Vec<Value> = blocks
            .iter()
            .filter(|b| b["type"] == "diff")
            .filter_map(|b| {
                let path = b["path"].as_str()?;
                let old = b["oldText"].as_str().map(str::as_bytes);
                let new = b["newText"].as_str().map(str::as_bytes);
                Some(file_change(&shown(path), old, new))
            })
            .collect();
        if !changes.is_empty() {
            return Some(files(changes));
        }
    }
    // Codex `applyPatchApproval`: path -> add / delete / update.
    if let Some(map) = args["changes"].as_object() {
        let changes: Vec<Value> = map
            .iter()
            .filter_map(|(path, change)| {
                let kind = change["type"]
                    .as_str()
                    .or_else(|| change.as_object()?.keys().next().map(String::as_str))?;
                let body = if change["type"].is_string() {
                    change
                } else {
                    &change[kind]
                };
                Some(match kind {
                    "add" => file_change(
                        &shown(path),
                        None,
                        Some(body["content"].as_str()?.as_bytes()),
                    ),
                    "delete" => {
                        let before = current(path).flatten();
                        file_change(&shown(path), Some(before.as_deref().unwrap_or(b"")), None)
                    }
                    "update" => {
                        let diff = body["unified_diff"].as_str().unwrap_or("");
                        let cut = diff.lines().count() > MAX_LINES;
                        let text: String = diff
                            .lines()
                            .filter(|l| !l.starts_with("--- ") && !l.starts_with("+++ "))
                            .take(MAX_LINES)
                            .map(|l| format!("{l}\n"))
                            .collect();
                        json!({
                            "path": shown(path),
                            "status": "modified",
                            "diff": text,
                            "added": text.lines().filter(|l| l.starts_with('+')).count(),
                            "removed": text.lines().filter(|l| l.starts_with('-')).count(),
                            "truncated": cut,
                            "binary": false,
                        })
                    }
                    _ => return None,
                })
            })
            .collect();
        return Some(files(changes));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        (dir, ws)
    }

    #[test]
    fn native_file_changes_diff_against_the_current_file() {
        let (_dir, ws) = project();
        let edit = native(
            &ws,
            "edit_file",
            &json!({"path":"a.txt","old_string":"two","new_string":"TWO"}),
        );
        let file = &edit["files"][0];
        assert_eq!(edit["kind"], "files");
        assert_eq!(file["status"], "modified");
        assert_eq!(file["added"], 1);
        assert_eq!(file["removed"], 1);
        assert!(file["diff"].as_str().unwrap().contains("-two\n+TWO\n"));

        let new = native(
            &ws,
            "write_file",
            &json!({"path":"b.txt","content":"hello\nworld\n"}),
        );
        assert_eq!(new["files"][0]["status"], "added");
        assert_eq!(new["files"][0]["diff"], "@@ -0,0 +1,2 @@\n+hello\n+world\n");
        let gone = native(&ws, "delete_file", &json!({"path":"a.txt"}));
        assert_eq!(gone["files"][0]["status"], "deleted");
        assert_eq!(gone["files"][0]["removed"], 3);
        let patch = native(
            &ws,
            "apply_patch",
            &json!({"patch":"--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n"}),
        );
        assert_eq!(patch["files"][0]["path"], "a.txt");
        assert_eq!(patch["files"][0]["added"], 1);
        // An edit that cannot apply has no preview; the tool reports why.
        assert!(native(
            &ws,
            "edit_file",
            &json!({"path":"a.txt","old_string":"missing","new_string":"x"})
        )
        .is_null());
        assert!(native(&ws, "read_file", &json!({"path":"a.txt"})).is_null());
    }

    #[test]
    fn close_edits_preview_what_the_edit_will_do() {
        let (dir, ws) = project();
        std::fs::write(
            dir.path().join("package.json"),
            "{\r\n  \"dependencies\": {\r\n    \"a\": \"1\"\r\n  }\r\n}\r\n",
        )
        .unwrap();
        // The edit uses Unix line endings; it still applies, so it is shown.
        let args = json!({"path":"package.json","old_string":"    \"a\": \"1\"\n","new_string":"    \"a\": \"1\",\n    \"left-pad\": \"1\"\n"});
        let edit = native(&ws, "edit_file", &args);
        assert_eq!(edit["files"][0]["added"], 2, "{edit}");
        let contents = native_contents(&ws, "edit_file", &args);
        let after = String::from_utf8(contents[0].2.clone().unwrap()).unwrap();
        assert!(after.contains("\"left-pad\": \"1\"\r\n"), "{after}");
        // Ambiguous, as the edit would be: no preview.
        std::fs::write(dir.path().join("twice.txt"), "x\nx\n").unwrap();
        assert!(native(
            &ws,
            "edit_file",
            &json!({"path":"twice.txt","old_string":"x","new_string":"y"})
        )
        .is_null());
    }

    #[test]
    fn commands_show_the_full_command_and_folder() {
        let (_dir, ws) = project();
        let shown = native(
            &ws,
            "exec",
            &json!({"command":"cargo test --lib","cwd":"crates/core"}),
        );
        assert_eq!(shown["kind"], "command");
        assert_eq!(shown["command"], "cargo test --lib");
        assert_eq!(
            shown["cwd"].as_str().unwrap(),
            ws.path.join("crates/core").to_str().unwrap()
        );
        let vendor = vendor(
            &ws.path,
            "command",
            &json!({"command":["npm","test"],"cwd":"/tmp/x"}),
        );
        assert_eq!(vendor["command"], "npm test");
        assert_eq!(vendor["cwd"], "/tmp/x");
    }

    #[test]
    fn vendor_file_changes_from_claude_acp_and_codex() {
        let (_dir, ws) = project();
        let claude = vendor(
            &ws.path,
            "file_change",
            &json!({"tool":"Edit","input":{"file_path":ws.path.join("a.txt"),"old_string":"one","new_string":"1"}}),
        );
        assert_eq!(claude["files"][0]["path"], "a.txt");
        assert!(claude["files"][0]["diff"]
            .as_str()
            .unwrap()
            .contains("-one\n+1\n"));
        let acp = vendor(
            &ws.path,
            "file_change",
            &json!({"content":[{"type":"diff","path":"new.rs","oldText":null,"newText":"fn main() {}\n"}]}),
        );
        assert_eq!(acp["files"][0]["status"], "added");
        let codex = vendor(
            &ws.path,
            "file_change",
            &json!({"changes":{"x.txt":{"add":{"content":"x\n"}},"a.txt":{"update":{"unified_diff":"@@ -1 +1 @@\n-one\n+uno\n"}}}}),
        );
        let files = codex["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f["status"] == "added"));
        assert!(files
            .iter()
            .any(|f| f["diff"].as_str().unwrap().contains("+uno")));
    }

    #[test]
    fn binary_files_are_named_but_not_diffed() {
        let shown = file_change("img.png", Some(&[0, 1, 2]), Some(&[0, 1]));
        assert_eq!(shown["binary"], true);
        assert_eq!(shown["diff"], "");
    }
}
