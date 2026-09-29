//! @-mentions: a fuzzy file and folder search for the composer, and the
//! context a native model reads for the files and folders a prompt names.
//!
//! The search walks the project the way `git` sees it (`.gitignore`,
//! `.ignore` and hidden files are skipped) and keeps the list for a few
//! seconds, so typing does not walk the tree on every key. Vendor CLIs get
//! `@path` in the prompt text instead, which they resolve themselves.
use crate::workspace::Workspace;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

/// A file or folder the prompt mentions, relative to the project.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mention {
    pub path: String,
    /// `file` or `dir`.
    #[serde(default)]
    pub kind: String,
}

/// An inspectable description of the exact file/folder attachment layer.
/// It deliberately excludes file contents; callers get paths, byte/line
/// bounds and the reason a selection was omitted.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ContextItem {
    pub path: String,
    pub kind: String,
    pub included: bool,
    pub reason: String,
    pub bytes: usize,
    pub total_bytes: Option<usize>,
    pub from_line: Option<usize>,
    pub to_line: Option<usize>,
    pub entries: Vec<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ContextPreview {
    pub items: Vec<ContextItem>,
    pub included_bytes: usize,
    /// Approximate token count for the exact bounded attachment text. This
    /// is a byte-based estimate, not a model tokenizer measurement.
    pub estimated_tokens: usize,
    pub truncated: bool,
}

struct ContextAssembly {
    prompt: Option<String>,
    preview: ContextPreview,
}

/// Mentions per prompt.
pub const MAX_MENTIONS: usize = 20;
/// Entries the search walks; larger trees are searched in part.
const MAX_ENTRIES: usize = 60_000;
const CACHE_FOR: Duration = Duration::from_secs(5);
/// Bytes of one mentioned file the model reads, and of all of them.
const FILE_BYTES: usize = 64 * 1024;
const TOTAL_BYTES: usize = 256 * 1024;
/// Entries listed for a mentioned folder.
const FOLDER_ENTRIES: usize = 200;

#[derive(Clone)]
struct Entry {
    path: String,
    dir: bool,
}
type Listing = Arc<(Vec<Entry>, bool)>;

fn cache() -> &'static Mutex<HashMap<PathBuf, (Instant, Listing)>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (Instant, Listing)>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn listing(root: &Path) -> Listing {
    if let Ok(map) = cache().lock() {
        if let Some((at, list)) = map.get(root) {
            if at.elapsed() < CACHE_FOR {
                return list.clone();
            }
        }
    }
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(true)
        .follow_links(false)
        .require_git(false)
        .sort_by_file_name(|a, b| a.cmp(b));
    for entry in builder.build().filter_map(Result::ok) {
        if entry.depth() == 0 {
            continue;
        }
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        entries.push(Entry {
            path: relative.to_string_lossy().into_owned(),
            dir: entry.file_type().is_some_and(|t| t.is_dir()),
        });
    }
    let list = Arc::new((entries, truncated));
    if let Ok(mut map) = cache().lock() {
        map.retain(|_, (at, _)| at.elapsed() < CACHE_FOR);
        map.insert(root.to_owned(), (Instant::now(), list.clone()));
    }
    list
}

/// How well `query` (lowercase, no spaces) matches `path`, or `None` when
/// its characters do not all appear in order. Matches in the file name, at
/// word starts and in runs score higher; long paths score lower.
pub fn score(query: &str, path: &str) -> Option<i64> {
    let query: Vec<char> = query.chars().collect();
    let text: Vec<char> = path.to_lowercase().chars().collect();
    let name_start = text
        .iter()
        .rposition(|c| *c == '/')
        .map_or(0, |index| index + 1);
    let mut matched = 0;
    let mut total = 0i64;
    let mut previous: Option<usize> = None;
    for (index, c) in text.iter().enumerate() {
        if matched < query.len() && *c == query[matched] {
            let mut points = 1;
            if previous.is_some_and(|p| p + 1 == index) {
                points += 5;
            }
            if index == 0 || matches!(text[index - 1], '/' | '_' | '-' | '.' | ' ') {
                points += 4;
            }
            if index >= name_start {
                points += 3;
            }
            total += points;
            previous = Some(index);
            matched += 1;
        }
    }
    if matched < query.len() {
        return None;
    }
    let name: String = text[name_start..].iter().collect();
    let wanted: String = query.iter().collect();
    if name == wanted || name.split('.').next() == Some(wanted.as_str()) {
        total += 40;
    } else if name.starts_with(&wanted) {
        total += 20;
    }
    Some(total - (text.len() as i64) / 6)
}

/// `{items: [{path, kind}], truncated}`: the best matches first. An empty
/// query lists the shallowest entries.
pub fn search(root: &Path, query: &str, limit: usize) -> Result<Value> {
    let limit = limit.clamp(1, 100);
    let query: String = query
        .trim()
        .trim_start_matches('@')
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    ensure!(query.len() <= 512, "Search text is too long");
    let list = listing(root);
    let (entries, truncated) = (&list.0, list.1);
    let mut hits: Vec<(i64, &Entry)> = if query.is_empty() {
        entries
            .iter()
            .map(|e| (-(e.path.matches('/').count() as i64), e))
            .collect()
    } else {
        entries
            .iter()
            .filter_map(|e| score(&query, &e.path).map(|s| (s, e)))
            .collect()
    };
    hits.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.path.len().cmp(&b.1.path.len()))
            .then(a.1.path.cmp(&b.1.path))
    });
    Ok(json!({
        "items": hits.into_iter().take(limit).map(|(_, e)| json!({
            "path": e.path,
            "kind": if e.dir { "dir" } else { "file" },
        })).collect::<Vec<_>>(),
        "truncated": truncated,
    }))
}

/// Whether `path` (relative to the project) is one of `scope`'s files or
/// inside one of its folders.
pub fn in_scope(scope: &[Mention], path: &str) -> bool {
    let path = path.trim_start_matches("./");
    scope.iter().any(|m| {
        let target = m.path.trim_start_matches("./").trim_end_matches('/');
        if m.kind == "dir" {
            target.is_empty()
                || target == "."
                || path == target
                || path.starts_with(&format!("{target}/"))
        } else {
            path == target
        }
    })
}

/// Check mentions from a request: inside the project, at most
/// `MAX_MENTIONS`, paths normalized.
pub fn validate(workspace: &Workspace, mentions: Vec<Mention>) -> Result<Vec<Mention>> {
    ensure!(
        mentions.len() <= MAX_MENTIONS,
        "Mention at most {MAX_MENTIONS} files or folders"
    );
    let mut out: Vec<Mention> = Vec::new();
    for mention in mentions {
        let relative = workspace.relative(&mention.path)?;
        let path = relative.to_string_lossy().into_owned();
        ensure!(path != "." || mention.kind == "dir", "Choose a file");
        let kind = if mention.kind == "dir" { "dir" } else { "file" };
        if !out.iter().any(|m| m.path == path) {
            out.push(Mention {
                path,
                kind: kind.into(),
            });
        }
    }
    Ok(out)
}

/// The attached context a native model reads after the prompt: each file's
/// current text (bounded) and each folder's entries. `None` without
/// mentions.
pub fn context(workspace: &Workspace, mentions: &[Mention]) -> Option<String> {
    assemble_context(workspace, mentions).prompt
}

/// Same bounded resolution used for task prompts, returned without source
/// text so the UI can explain what ShadowCode will attach before sending.
pub fn preview(workspace: &Workspace, mentions: &[Mention]) -> ContextPreview {
    assemble_context(workspace, mentions).preview
}

fn assemble_context(workspace: &Workspace, mentions: &[Mention]) -> ContextAssembly {
    if mentions.is_empty() {
        return ContextAssembly {
            prompt: None,
            preview: ContextPreview::default(),
        };
    }
    let mut out = String::from(
        "The user attached these project files and folders with @-mentions. Contents were read when the task started; read a file again before editing it.\n",
    );
    let mut budget = TOTAL_BYTES;
    let mut preview = ContextPreview::default();
    for mention in mentions.iter().take(MAX_MENTIONS) {
        if mention.kind == "dir" {
            out.push_str(&format!("\n<folder path=\"{}\">\n", mention.path));
            match workspace.list(&mention.path) {
                Ok(entries) => {
                    let included: Vec<String> = entries
                        .iter()
                        .take(FOLDER_ENTRIES)
                        .map(|entry| {
                            let suffix = if entry.kind == "dir" { "/" } else { "" };
                            format!("{}{suffix}", entry.path)
                        })
                        .collect();
                    for entry in &included {
                        out.push_str(entry);
                        out.push('\n');
                    }
                    let truncated = entries.len() > FOLDER_ENTRIES;
                    if entries.len() > FOLDER_ENTRIES {
                        out.push_str(&format!(
                            "[{} more entries; list the folder for the rest]\n",
                            entries.len() - FOLDER_ENTRIES
                        ));
                    }
                    preview.truncated |= truncated;
                    preview.items.push(ContextItem {
                        path: mention.path.clone(),
                        kind: "dir".into(),
                        included: true,
                        reason: "Explicit @mention; folder names are attached, not file contents."
                            .into(),
                        bytes: included.iter().map(String::len).sum(),
                        total_bytes: None,
                        from_line: None,
                        to_line: None,
                        entries: included,
                        truncated,
                    });
                }
                Err(_) => {
                    out.push_str("[Could not list this folder]\n");
                    preview.items.push(ContextItem {
                        path: mention.path.clone(),
                        kind: "dir".into(),
                        included: false,
                        reason: "Folder could not be read when this preview was prepared.".into(),
                        bytes: 0,
                        total_bytes: None,
                        from_line: None,
                        to_line: None,
                        entries: Vec::new(),
                        truncated: false,
                    });
                }
            }
            out.push_str("</folder>\n");
            continue;
        }
        out.push_str(&format!("\n<file path=\"{}\">\n", mention.path));
        // A mentioned secret file (by name, spelling or symlink) is never put
        // into model context, like read_file.
        if workspace.is_secret_target(&mention.path) {
            out.push_str("[Not included: secret files are not sent to the model]\n");
            out.push_str("</file>\n");
            preview.items.push(ContextItem {
                path: mention.path.clone(),
                kind: "file".into(),
                included: false,
                reason: "Secret files (.env, credentials, keys) are never attached.".into(),
                bytes: 0,
                total_bytes: None,
                from_line: None,
                to_line: None,
                entries: Vec::new(),
                truncated: false,
            });
            continue;
        }
        match workspace.read(&mention.path) {
            Ok(_file) if budget == 0 => {
                out.push_str("[Not included: the attached context is full; read the file]\n");
                preview.truncated = true;
                preview.items.push(ContextItem {
                    path: mention.path.clone(),
                    kind: "file".into(),
                    included: false,
                    reason: "The 256 KiB total attachment limit was already reached.".into(),
                    bytes: 0,
                    total_bytes: None,
                    from_line: None,
                    to_line: None,
                    entries: Vec::new(),
                    truncated: true,
                });
            }
            Ok(file) => {
                let take = FILE_BYTES.min(budget);
                let text = crate::tools::truncate(&file.content, take);
                budget -= text.len();
                // Secret-looking tokens are replaced before model context, as
                // read_file does.
                out.push_str(&crate::redaction::redact_text(text).text);
                let truncated = text.len() < file.content.len();
                let line_count = text.lines().count();
                if !text.ends_with('\n') {
                    out.push('\n');
                }
                if truncated {
                    out.push_str(&format!(
                        "[Truncated after {} of {} bytes; read the file for the rest]\n",
                        text.len(),
                        file.content.len()
                    ));
                }
                preview.included_bytes += text.len();
                preview.truncated |= truncated;
                preview.items.push(ContextItem {
                    path: mention.path.clone(),
                    kind: "file".into(),
                    included: true,
                    reason: "Explicit @mention; current file text is attached within per-file and total byte limits.".into(),
                    bytes: text.len(),
                    total_bytes: Some(file.content.len()),
                    from_line: (line_count > 0).then_some(1),
                    to_line: (line_count > 0).then_some(line_count),
                    entries: Vec::new(),
                    truncated,
                });
            }
            Err(_) => {
                out.push_str("[Could not read this file]\n");
                preview.items.push(ContextItem {
                    path: mention.path.clone(),
                    kind: "file".into(),
                    included: false,
                    reason: "File could not be read when this preview was prepared.".into(),
                    bytes: 0,
                    total_bytes: None,
                    from_line: None,
                    to_line: None,
                    entries: Vec::new(),
                    truncated: false,
                });
            }
        }
        out.push_str("</file>\n");
    }
    // Keep this estimate tied to the very same bounded prompt text returned to
    // native execution. Provider tokenizers differ, so the UI labels it as an
    // estimate rather than presenting it as exact usage.
    preview.estimated_tokens = out.len().div_ceil(3);
    ContextAssembly {
        prompt: Some(out),
        preview,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src/components")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        fs::write(root.join("src/components/Composer.tsx"), "export {}\n").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("README.md"), "# Demo\n").unwrap();
        fs::write(root.join("target/debug/build.log"), "x").unwrap();
        dir
    }

    fn paths(value: &Value) -> Vec<String> {
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["path"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn fuzzy_search_ranks_names_and_skips_ignored_files() {
        let dir = project();
        let found = search(dir.path(), "compo", 10).unwrap();
        let list = paths(&found);
        assert_eq!(list[0], "src/components");
        assert!(list.contains(&"src/components/Composer.tsx".to_owned()));
        // Characters in order, not adjacent.
        assert_eq!(
            paths(&search(dir.path(), "smrs", 10).unwrap())[0],
            "src/main.rs"
        );
        assert!(paths(&search(dir.path(), "build", 10).unwrap()).is_empty());
        assert!(!paths(&search(dir.path(), "", 50).unwrap())
            .iter()
            .any(|p| p.starts_with("target") || p.starts_with(".git")));
        let kinds = search(dir.path(), "src", 10).unwrap();
        assert_eq!(kinds["items"][0], json!({"path":"src","kind":"dir"}));
    }

    #[test]
    fn scores_prefer_file_names_and_word_starts() {
        assert!(score("main", "src/main.rs") > score("main", "domain/other.rs"));
        assert!(score("comp", "src/Composer.tsx") > score("comp", "src/deep/xcomputer.ts"));
        assert_eq!(score("zzz", "src/main.rs"), None);
    }

    #[test]
    fn context_includes_file_text_and_folder_entries() {
        let dir = project();
        let ws = Workspace::open(dir.path()).unwrap();
        let mentions = validate(
            &ws,
            vec![
                Mention {
                    path: "src/main.rs".into(),
                    kind: "file".into(),
                },
                Mention {
                    path: format!("{}/src", ws.path.display()),
                    kind: "dir".into(),
                },
                Mention {
                    path: "src/main.rs".into(),
                    kind: "file".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(mentions.len(), 2, "duplicates collapse");
        assert_eq!(mentions[1].path, "src");
        let text = context(&ws, &mentions).unwrap();
        assert!(text.contains("<file path=\"src/main.rs\">\nfn main() {}\n</file>"));
        assert!(text.contains("<folder path=\"src\">\nsrc/components/\nsrc/main.rs\n</folder>"));
        let inventory = preview(&ws, &mentions);
        assert_eq!(inventory.items.len(), 2);
        assert_eq!(inventory.items[0].path, "src/main.rs");
        assert!(inventory.items[0].included);
        assert_eq!(inventory.estimated_tokens, text.len().div_ceil(3));
        assert_eq!(inventory.items[0].from_line, Some(1));
        assert_eq!(inventory.items[0].to_line, Some(1));
        assert_eq!(inventory.items[0].total_bytes, Some("fn main() {}\n".len()));
        assert_eq!(
            inventory.items[1].entries,
            ["src/components/", "src/main.rs"]
        );
        assert!(!inventory.truncated);

        let missing = preview(
            &ws,
            &[Mention {
                path: "missing.rs".into(),
                kind: "file".into(),
            }],
        );
        assert!(!missing.items[0].included);
        assert!(missing.items[0].reason.contains("could not be read"));
        assert!(context(&ws, &[]).is_none());
        assert!(validate(
            &ws,
            vec![Mention {
                path: "../outside".into(),
                kind: "file".into()
            }]
        )
        .is_err());
    }

    #[test]
    fn mentioned_secret_files_are_not_attached_and_tokens_are_redacted() {
        let dir = project();
        fs::write(dir.path().join(".env"), "DB_PASSWORD=hunter2plaintext\n").unwrap();
        std::os::unix::fs::symlink(".env", dir.path().join("notes.txt")).unwrap();
        let token = format!("{}{}", "ghp_", "A".repeat(36));
        fs::write(dir.path().join("deploy.md"), format!("push {token}\n")).unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        let mentions = validate(
            &ws,
            vec![
                Mention {
                    path: ".env".into(),
                    kind: "file".into(),
                },
                Mention {
                    path: "notes.txt".into(),
                    kind: "file".into(),
                },
                Mention {
                    path: "deploy.md".into(),
                    kind: "file".into(),
                },
            ],
        )
        .unwrap();
        let text = context(&ws, &mentions).unwrap();
        assert!(!text.contains("hunter2plaintext"), "secret leaked: {text}");
        assert!(!text.contains(&token), "token leaked: {text}");
        assert!(text.contains("secret files are not sent"), "{text}");
        assert!(text.contains("[redacted secret]"), "{text}");
    }
}
