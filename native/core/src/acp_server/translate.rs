//! Engine events → ACP `session/update` payloads. The same translator
//! streams a live turn and replays a stored conversation for `session/load`,
//! so both show identical tool calls, plans and text.
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Text sent in one tool-call content block or diff side.
const CONTENT_LIMIT: usize = 64_000;
/// `rawInput` / `rawOutput` are omitted beyond this serialized size.
const RAW_LIMIT: usize = 64_000;

fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    format!(
        "{}\n… ({} more bytes)",
        crate::tools::truncate(text, limit),
        text.len() - crate::tools::truncate(text, limit).len()
    )
}

fn raw(value: &Value) -> Option<Value> {
    (!value.is_null() && value.to_string().len() <= RAW_LIMIT).then(|| value.clone())
}

fn text_block(text: &str) -> Value {
    json!({"type":"text","text":text})
}

pub(crate) fn message_chunk(text: &str) -> Value {
    json!({"sessionUpdate":"agent_message_chunk","content":text_block(text)})
}

pub(crate) fn thought_chunk(text: &str) -> Value {
    json!({"sessionUpdate":"agent_thought_chunk","content":text_block(text)})
}

/// ACP `ToolKind` for a ShadowCode tool (unknown and extension tools: other).
pub(crate) fn tool_kind(tool: &str) -> &'static str {
    match tool {
        "read_file" | "list_files" | "view_image" | "system_info" | "git_status" | "git_diff"
        | "git_log" | "background_list" | "background_output" | "get_diagnostics"
        | "get_type_signature" | "repo_map" | "mcp_sqlite_tables" | "mcp_sqlite_query" => "read",
        "search_files" | "search_text" | "search_symbol" | "search_code" | "workspace_symbols"
        | "goto_definition" | "find_references" => "search",
        "write_file" | "edit_file" | "apply_patch" | "create_directory" => "edit",
        "move_file" => "move",
        "delete_file" => "delete",
        "exec" | "background_start" | "background_stop" | "git_add" | "git_commit"
        | "git_branch" | "git_checkout" => "execute",
        "web_fetch" | "web_search" => "fetch",
        "update_plan" => "think",
        _ => "other",
    }
}

fn arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args[key].as_str().unwrap_or("")
}

/// A short, human title such as "Read src/main.rs" or "Run `cargo test`".
pub(crate) fn tool_title(tool: &str, args: &Value) -> String {
    let path = arg(args, "path");
    let with = |verb: &str, detail: &str| {
        if detail.is_empty() {
            verb.to_owned()
        } else {
            format!("{verb} {}", clip(detail, 200))
        }
    };
    match tool {
        "read_file" => with("Read", path),
        "list_files" => with("List", if path.is_empty() { "." } else { path }),
        "view_image" => with("View image", path),
        "write_file" => with("Write", path),
        "edit_file" => with("Edit", path),
        "apply_patch" => with("Apply patch", path),
        "create_directory" => with("Create folder", path),
        "delete_file" => with("Delete", path),
        "move_file" => format!("Move {} → {}", arg(args, "src"), arg(args, "dest")),
        "exec" | "background_start" => format!("Run `{}`", clip(arg(args, "command"), 200)),
        "search_files" | "search_text" | "search_symbol" | "search_code" | "workspace_symbols" => {
            with("Search", &format!("\"{}\"", clip(arg(args, "query"), 120)))
        }
        "web_fetch" => with("Fetch", arg(args, "url")),
        "web_search" => with("Web search", arg(args, "query")),
        "update_plan" => "Update plan".into(),
        "git_commit" => with("Commit", arg(args, "message")),
        "git_checkout" => with("Checkout", arg(args, "ref")),
        _ => with(tool, path),
    }
}

fn absolute(workspace: &Path, path: &str) -> String {
    let path = Path::new(path);
    if path.is_absolute() {
        path.display().to_string()
    } else {
        workspace.join(path).display().to_string()
    }
}

fn locations(workspace: &Path, tool: &str, args: &Value) -> Vec<Value> {
    let mut found = Vec::new();
    let mut push = |path: &str, line: Option<u64>| {
        if !path.is_empty() {
            let mut location = json!({"path":absolute(workspace, path)});
            if let Some(line) = line {
                location["line"] = json!(line);
            }
            found.push(location);
        }
    };
    if tool == "move_file" {
        push(arg(args, "src"), None);
        push(arg(args, "dest"), None);
    } else if !matches!(
        tool,
        "exec" | "background_start" | "web_fetch" | "web_search"
    ) {
        push(
            arg(args, "path"),
            args["line"].as_u64().or_else(|| args["offset"].as_u64()),
        );
    }
    found
}

/// Proposed edits shown as ACP diffs (`oldText: null` marks a whole new text).
fn diffs(workspace: &Path, tool: &str, args: &Value) -> Vec<Value> {
    let path = arg(args, "path");
    match tool {
        "edit_file" if !path.is_empty() => vec![json!({
            "type":"diff",
            "path":absolute(workspace, path),
            "oldText":clip(arg(args, "old_string"), CONTENT_LIMIT),
            "newText":clip(arg(args, "new_string"), CONTENT_LIMIT),
        })],
        "write_file" if !path.is_empty() => vec![json!({
            "type":"diff",
            "path":absolute(workspace, path),
            "oldText":null,
            "newText":clip(arg(args, "content"), CONTENT_LIMIT),
        })],
        "apply_patch" => vec![json!({
            "type":"content",
            "content":text_block(&format!("```diff\n{}\n```", clip(arg(args, "patch"), CONTENT_LIMIT))),
        })],
        _ => Vec::new(),
    }
}

fn output_text(tool: &str, payload: &Value) -> Option<String> {
    if payload["success"] != true {
        let error = payload["error"].as_str().filter(|e| !e.is_empty());
        return Some(clip(
            error.unwrap_or(payload["output_preview"].as_str().unwrap_or("Tool failed")),
            CONTENT_LIMIT,
        ));
    }
    let output = &payload["output"];
    match tool_kind(tool) {
        // The location (or diff) already shows these; the full read is noise.
        "read" if tool == "read_file" || tool == "view_image" => None,
        "edit" | "move" | "delete" | "think" => None,
        _ => {
            let stdout = output["stdout"].as_str().unwrap_or("");
            let stderr = output["stderr"].as_str().unwrap_or("");
            let text = if !stdout.is_empty() || !stderr.is_empty() {
                format!(
                    "{stdout}{}{stderr}",
                    if stdout.is_empty() || stderr.is_empty() {
                        ""
                    } else {
                        "\n"
                    }
                )
            } else {
                payload["output_preview"].as_str().unwrap_or("").to_owned()
            };
            (!text.trim().is_empty())
                .then(|| format!("```\n{}\n```", clip(text.trim_end(), CONTENT_LIMIT)))
        }
    }
}

/// Readable text for an approval preview (`approvals::preview` shapes), shown
/// when the tool call has no diff of its own.
pub(crate) fn preview_text(preview: &Value) -> Option<String> {
    let text = match preview["kind"].as_str()? {
        "files" => preview["files"]
            .as_array()?
            .iter()
            .map(|file| {
                format!(
                    "```diff\n--- {} ({})\n{}\n```\n",
                    file["path"].as_str().unwrap_or(""),
                    file["status"].as_str().unwrap_or("modified"),
                    clip(file["diff"].as_str().unwrap_or(""), CONTENT_LIMIT)
                )
            })
            .collect(),
        "command" => format!(
            "Runs in {}",
            preview["cwd"]
                .as_str()
                .filter(|c| !c.is_empty())
                .unwrap_or(".")
        ),
        "move" => format!(
            "Move {} → {}",
            preview["from"].as_str().unwrap_or(""),
            preview["to"].as_str().unwrap_or("")
        ),
        "folder" => format!("Create folder {}", preview["path"].as_str().unwrap_or("")),
        _ => return None,
    };
    (!text.trim().is_empty()).then_some(text)
}

fn plan_entries(plan: &Value) -> Vec<Value> {
    plan["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|step| {
            json!({
                "content": step["title"].as_str().unwrap_or(""),
                "priority": "medium",
                "status": match step["status"].as_str().unwrap_or("") {
                    "done" | "completed" => "completed",
                    "running" | "in_progress" => "in_progress",
                    _ => "pending",
                },
            })
        })
        .collect()
}

/// An open (started, not yet finished) tool call.
#[derive(Clone, Debug)]
pub(crate) struct OpenCall {
    pub id: String,
    pub tool: String,
    pub arguments: Value,
}

pub(crate) struct Translator {
    workspace: PathBuf,
    replay: bool,
    /// Text already sent per engine message id.
    streamed: HashMap<String, String>,
    open: Vec<OpenCall>,
}

impl Translator {
    pub fn new(workspace: PathBuf, replay: bool) -> Self {
        Self {
            workspace,
            replay,
            streamed: HashMap::new(),
            open: Vec::new(),
        }
    }
    /// The most recent open call of `tool` (an approval refers to it).
    pub fn open_call(&self, tool: &str) -> Option<&OpenCall> {
        self.open.iter().rev().find(|call| call.tool == tool)
    }
    /// The `tool_call` payload fields for a call, without `sessionUpdate`.
    pub fn describe(&self, tool: &str, arguments: &Value) -> Value {
        json!({
            "title": tool_title(tool, arguments),
            "kind": tool_kind(tool),
            "locations": locations(&self.workspace, tool, arguments),
            "content": diffs(&self.workspace, tool, arguments),
            "rawInput": raw(arguments),
        })
    }
    /// Zero or more `update` objects for one stored engine event.
    pub fn updates(&mut self, event: &Value) -> Vec<Value> {
        let payload = &event["payload"];
        let text = payload["text"].as_str().unwrap_or("");
        match event["type"].as_str().unwrap_or("") {
            "user.message" if self.replay && !text.is_empty() => {
                vec![json!({"sessionUpdate":"user_message_chunk","content":text_block(text)})]
            }
            "model.stream" if !text.is_empty() => {
                let id = payload["message_id"].as_str().unwrap_or("").to_owned();
                self.streamed.entry(id).or_default().push_str(text);
                vec![message_chunk(text)]
            }
            "model.delta" if payload["complete"] == true && !text.is_empty() => {
                let id = payload["message_id"].as_str().unwrap_or("").to_owned();
                let sent = self.streamed.insert(id, text.to_owned());
                let rest = match sent {
                    None => text,
                    Some(sent) => text.strip_prefix(sent.as_str()).unwrap_or(""),
                };
                if rest.is_empty() {
                    Vec::new()
                } else {
                    vec![message_chunk(rest)]
                }
            }
            "tool.started" => {
                let tool = payload["tool"].as_str().unwrap_or("tool").to_owned();
                let id = payload["call_id"].as_str().unwrap_or("").to_owned();
                let arguments = payload["arguments"].clone();
                let mut update = self.describe(&tool, &arguments);
                update["sessionUpdate"] = json!("tool_call");
                update["toolCallId"] = json!(id);
                update["status"] = json!("in_progress");
                self.open.push(OpenCall {
                    id,
                    tool,
                    arguments,
                });
                vec![update]
            }
            "tool.completed" => {
                let tool = payload["tool"].as_str().unwrap_or("tool");
                let id = payload["call_id"].as_str().unwrap_or("");
                let status = if payload["success"] == true {
                    "completed"
                } else {
                    "failed"
                };
                let known = self.open.iter().position(|call| call.id == id);
                let mut update = match known {
                    Some(index) => {
                        self.open.remove(index);
                        json!({"sessionUpdate":"tool_call_update"})
                    }
                    None => {
                        let mut update = self.describe(tool, &Value::Null);
                        update["sessionUpdate"] = json!("tool_call");
                        update
                    }
                };
                update["toolCallId"] = json!(id);
                update["status"] = json!(status);
                if let Some(text) = output_text(tool, payload) {
                    update["content"] = json!([{"type":"content","content":text_block(&text)}]);
                }
                if let Some(output) = raw(&payload["output"]) {
                    update["rawOutput"] = output;
                }
                vec![update]
            }
            "plan.updated" => {
                vec![json!({"sessionUpdate":"plan","entries":plan_entries(&payload["plan"])})]
            }
            "agent.warning" if !text.is_empty() => vec![thought_chunk(&format!("{text}\n"))],
            // An editor's task never pauses when stuck: the agent is told to
            // change course, and the editor sees why.
            "agent.stuck" if !text.is_empty() => vec![thought_chunk(&format!("{text}\n"))],
            "routing.selected" | "routing.fallback" => {
                let name = payload["model_name"]
                    .as_str()
                    .or_else(|| payload["model"].as_str())
                    .unwrap_or("");
                if name.is_empty() {
                    Vec::new()
                } else {
                    vec![thought_chunk(&format!("Model: {name}\n"))]
                }
            }
            "model.retry" => vec![thought_chunk("Retrying the model request…\n")],
            // A spending limit is answered in ShadowCode (desktop, remote or
            // `shadowcode spending`); the editor sees why the task waits.
            "spend.notice" | "spend.unknown" | "spend.limit_resolved" if !text.is_empty() => {
                vec![thought_chunk(&format!("{text}\n"))]
            }
            "spend.limit_reached" => vec![thought_chunk(&format!(
                "{} {} Continue or stop it in ShadowCode.\n",
                payload["title"]
                    .as_str()
                    .unwrap_or("Spending limit reached."),
                text
            ))],
            "context.compacted" => vec![thought_chunk(
                "Compacted earlier conversation to fit the context window.\n",
            )],
            _ => Vec::new(),
        }
    }
    /// Close calls a cancelled or failed turn left open.
    pub fn close_open(&mut self, status: &str) -> Vec<Value> {
        self.open
            .drain(..)
            .map(|call| {
                json!({"sessionUpdate":"tool_call_update","toolCallId":call.id,"status":status})
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, payload: Value) -> Value {
        json!({"type":kind,"payload":payload})
    }

    #[test]
    fn streamed_text_is_not_repeated_by_the_final_message() {
        let mut t = Translator::new("/w".into(), false);
        let a = t.updates(&event(
            "model.stream",
            json!({"text":"Hel","message_id":"m"}),
        ));
        assert_eq!(a[0]["content"]["text"], "Hel");
        let b = t.updates(&event(
            "model.delta",
            json!({"text":"Hello","message_id":"m","complete":true}),
        ));
        assert_eq!(b[0]["content"]["text"], "lo");
        assert!(t
            .updates(&event(
                "model.delta",
                json!({"text":"partial","message_id":"x","complete":false})
            ))
            .is_empty());
        let whole = t.updates(&event(
            "model.delta",
            json!({"text":"Whole","message_id":"n","complete":true}),
        ));
        assert_eq!(whole[0]["content"]["text"], "Whole");
    }

    #[test]
    fn tools_carry_kind_locations_and_diffs() {
        let mut t = Translator::new("/w".into(), false);
        let started = t.updates(&event(
            "tool.started",
            json!({"tool":"edit_file","call_id":"c1","arguments":{"path":"src/a.rs","old_string":"a","new_string":"b"}}),
        ));
        assert_eq!(started[0]["sessionUpdate"], "tool_call");
        assert_eq!(started[0]["kind"], "edit");
        assert_eq!(started[0]["locations"][0]["path"], "/w/src/a.rs");
        assert_eq!(started[0]["content"][0]["type"], "diff");
        assert_eq!(started[0]["content"][0]["oldText"], "a");
        assert_eq!(t.open_call("edit_file").unwrap().id, "c1");
        let done = t.updates(&event(
            "tool.completed",
            json!({"tool":"edit_file","call_id":"c1","success":true,"output":{"ok":true}}),
        ));
        assert_eq!(done[0]["sessionUpdate"], "tool_call_update");
        assert_eq!(done[0]["status"], "completed");
        assert!(done[0].get("content").is_none());
        assert!(t.open_call("edit_file").is_none());
        let failed = t.updates(&event(
            "tool.completed",
            json!({"tool":"exec","call_id":"c2","success":false,"error":"denied"}),
        ));
        assert_eq!(failed[0]["sessionUpdate"], "tool_call");
        assert_eq!(failed[0]["status"], "failed");
        assert_eq!(failed[0]["content"][0]["content"]["text"], "denied");
    }

    #[test]
    fn plans_and_replayed_user_messages() {
        let mut live = Translator::new("/w".into(), false);
        assert!(live
            .updates(&event("user.message", json!({"text":"hi"})))
            .is_empty());
        let mut replay = Translator::new("/w".into(), true);
        assert_eq!(
            replay.updates(&event("user.message", json!({"text":"hi"})))[0]["sessionUpdate"],
            "user_message_chunk"
        );
        let plan = live.updates(&event(
            "plan.updated",
            json!({"plan":{"steps":[{"title":"a","status":"done"},{"title":"b","status":"running"},{"title":"c","status":"blocked"}]}}),
        ));
        let statuses: Vec<_> = plan[0]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, ["completed", "in_progress", "pending"]);
        let stuck = live.updates(&event(
            "agent.stuck",
            json!({"text":"The agent seems stuck.","paused":false}),
        ));
        assert_eq!(stuck[0]["sessionUpdate"], "agent_thought_chunk");
        assert_eq!(stuck[0]["content"]["text"], "The agent seems stuck.\n");
        assert_eq!(tool_kind("exec"), "execute");
        let diff = preview_text(&json!({"kind":"files","files":[{"path":"a.rs","status":"modified","diff":"@@ -1 +1 @@\n-a\n+b"}]})).unwrap();
        assert!(diff.contains("--- a.rs (modified)") && diff.contains("+b"));
        assert_eq!(
            preview_text(&json!({"kind":"command","command":"ls","cwd":"src"})).unwrap(),
            "Runs in src"
        );
        assert!(preview_text(&Value::Null).is_none());
        assert_eq!(tool_title("exec", &json!({"command":"ls"})), "Run `ls`");
    }
}
