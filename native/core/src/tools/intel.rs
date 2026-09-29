//! Code-intelligence tools (repo_map, search_code, LSP-backed definitions,
//! references and diagnostics) and the post-edit diagnostics hook.
use super::*;

const MAX_BASELINE_BYTES: u64 = 512_000;

impl ToolExecutor {
    pub(super) fn lsp_env(&self) -> crate::lsp::Env {
        crate::lsp::Env::new(
            &self.workspace.path,
            &self.config,
            self.code_intel_dir.as_deref(),
        )
    }

    /// Before an edit: the text of each file it will change that a language
    /// server could check afterwards (None for a file that does not exist yet).
    pub(super) fn edit_baseline(&self, call: &ToolCall) -> Option<HashMap<String, Option<String>>> {
        let paths: Vec<String> = match call.name.as_str() {
            "write_file" | "edit_file" => vec![call.arguments["path"].as_str()?.to_owned()],
            "apply_patch" => patch_paths(
                call.arguments["patch"]
                    .as_str()
                    .or_else(|| call.arguments["diff"].as_str())?,
                call.arguments["path"].as_str(),
            ),
            _ => return None,
        };
        let env = self.lsp_env();
        let mut out = HashMap::new();
        for path in paths.into_iter().take(8) {
            let Ok(rel) = self.workspace.relative(&path) else {
                continue;
            };
            let rel = rel.to_string_lossy().into_owned();
            if crate::redaction::is_secret_path(&rel) || !crate::lsp::handles(&env, &rel) {
                continue;
            }
            let full = self.workspace.path.join(&rel);
            let before = match std::fs::metadata(&full) {
                Err(_) => None,
                Ok(m) if m.is_file() && m.len() <= MAX_BASELINE_BYTES => {
                    match std::fs::read_to_string(&full) {
                        Ok(text) => Some(text),
                        Err(_) => continue,
                    }
                }
                Ok(_) => continue,
            };
            out.insert(rel, before);
        }
        (!out.is_empty()).then_some(out)
    }

    /// After a successful edit: errors it introduced, per the language server.
    pub(super) async fn edit_diagnostics(
        &self,
        baseline: &HashMap<String, Option<String>>,
        output: &Value,
    ) -> Option<Value> {
        let files: Vec<(String, Option<String>)> = output["paths"]
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|p| baseline.get(p).map(|before| (p.to_owned(), before.clone())))
            .collect();
        if files.is_empty() {
            return None;
        }
        crate::lsp::check_edits(&self.lsp_env(), &files).await
    }

    /// Open a file the agent just read in its language server (background).
    pub(super) fn warm_file(&self, output: &Value) {
        if let Some(path) = output["path"].as_str() {
            crate::lsp::warm(self.lsp_env(), path.to_owned());
        }
    }

    pub(super) async fn intel(&self, call: &ToolCall) -> Result<Value> {
        let args = &call.arguments;
        let root = self.workspace.path.clone();
        match call.name.as_str() {
            "repo_map" => {
                let intel = crate::code_intel::CodeIntelConfig::lenient(&self.config);
                let default = if intel.repo_map_tokens == 0 {
                    1024
                } else {
                    intel.repo_map_tokens
                };
                let max_tokens = integer(args, "max_tokens", default.clamp(128, 8192), 128, 8192)?;
                let query = args["query"].as_str().unwrap_or("").to_owned();
                let mut focus = Vec::new();
                if let Some(paths) = args["paths"].as_array() {
                    ensure!(paths.len() <= 50, "Give at most 50 focus paths");
                    for path in paths {
                        let path = path.as_str().context("Each path must be a string")?;
                        focus.push(
                            self.workspace
                                .relative(path)?
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                }
                focus.extend(crate::symbol_index::recent_edits(&root, 12));
                tokio::task::spawn_blocking(move || {
                    crate::code_intel::repo_map::build_json(&root, &focus, &query, max_tokens)
                })
                .await
                .context("Tool worker stopped unexpectedly")?
            }
            "search_code" => {
                let request = crate::code_intel::search::Request {
                    query: string(args, "query")?.to_owned(),
                    path: args["path"].as_str().unwrap_or("").to_owned(),
                    max_hits: integer(args, "max_hits", 10, 1, 50)?,
                };
                let semantic = self.code_intel_dir.as_deref().and_then(|dir| {
                    crate::code_intel::embeddings::Semantic::resolve(dir, &self.config)
                });
                crate::code_intel::search::search_code(root, request, semantic).await
            }
            "goto_definition" | "find_references" => self.locate(call).await,
            "get_diagnostics" => self.diagnostics(string(args, "path")?).await,
            _ => bail!("Unknown tool: {}", call.name),
        }
    }

    async fn locate(&self, call: &ToolCall) -> Result<Value> {
        let args = &call.arguments;
        let max_hits = integer(args, "max_hits", 40, 1, 80)?;
        let mut symbol = args["query"]
            .as_str()
            .or_else(|| args["symbol"].as_str())
            .unwrap_or("")
            .to_owned();
        let mut lsp_note = None;
        if let Some(path) = args["path"].as_str() {
            let line = integer(args, "line", 0, 1, 10_000_000)?;
            let column = integer(args, "column", 1, 1, 100_000)?;
            let rel = self
                .workspace
                .relative(path)?
                .to_string_lossy()
                .into_owned();
            let kind = if call.name == "goto_definition" {
                crate::lsp::Locate::Definition
            } else {
                crate::lsp::Locate::References
            };
            match crate::lsp::locate(&self.lsp_env(), kind, &rel, line, column).await {
                Ok(Some(found)) if found["ok"] == true => return Ok(found),
                Ok(Some(found)) => lsp_note = found["note"].as_str().map(str::to_owned),
                Ok(None) => {}
                Err(error) => lsp_note = Some(format!("Language server unavailable: {error:#}")),
            }
            if symbol.is_empty() {
                let text = self.workspace.read(&rel)?.content;
                symbol = identifier_at(&text, line, column).unwrap_or_default();
            }
        }
        ensure!(
            !symbol.is_empty(),
            "Give a symbol name, or a path and line (and column) inside a file"
        );
        let name = call.name.clone();
        let root = self.workspace.path.clone();
        let mut result = tokio::task::spawn_blocking(move || {
            if name == "goto_definition" {
                crate::intelligence::goto_definition(&root, &symbol, max_hits)
            } else {
                crate::intelligence::find_references(&root, &symbol, max_hits)
            }
        })
        .await
        .context("Tool worker stopped unexpectedly")??;
        if let Some(note) = lsp_note {
            result["lsp_note"] = json!(note);
        }
        Ok(result)
    }

    async fn diagnostics(&self, path: &str) -> Result<Value> {
        let rel = self
            .workspace
            .relative(path)?
            .to_string_lossy()
            .into_owned();
        ensure!(
            !crate::redaction::is_secret_path(&rel),
            "Secret paths cannot be inspected"
        );
        let wait = Duration::from_secs(
            self.config
                .agent
                .tool_timeout_sec
                .saturating_sub(5)
                .clamp(5, 30),
        );
        let lsp = crate::lsp::file_diagnostics(&self.lsp_env(), &rel, wait).await;
        let rust = rel.ends_with(".rs") && crate::intelligence::rust_analyzer_available();
        match lsp {
            Ok(Some(result)) if result["ok"] == true || !rust => Ok(result),
            Ok(Some(result)) => {
                let mut fallback =
                    crate::intelligence::get_diagnostics(&self.workspace.path, &rel).await?;
                fallback["lsp"] = result;
                Ok(fallback)
            }
            Ok(None) if rust => {
                crate::intelligence::get_diagnostics(&self.workspace.path, &rel).await
            }
            Ok(None) => Ok(json!({
                "ok": false,
                "path": rel,
                "error": "No language server handles this file type, or language servers are turned off (code_intel.lsp).",
            })),
            Err(error) if rust => {
                let mut fallback =
                    crate::intelligence::get_diagnostics(&self.workspace.path, &rel).await?;
                fallback["lsp_error"] = json!(format!("{error:#}"));
                Ok(fallback)
            }
            Err(error) => Err(error),
        }
    }
}

/// Files a unified diff or `*** Begin Patch` block touches.
pub(super) fn patch_paths(patch: &str, path: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in patch.lines() {
        let found = line
            .strip_prefix("+++ ")
            .or_else(|| line.strip_prefix("--- "))
            .map(|p| {
                let p = p.split('\t').next().unwrap_or(p).trim();
                p.strip_prefix("a/")
                    .or_else(|| p.strip_prefix("b/"))
                    .unwrap_or(p)
            })
            .filter(|p| *p != "/dev/null")
            .or_else(|| line.strip_prefix("*** Update File: "))
            .or_else(|| line.strip_prefix("*** Add File: "))
            .or_else(|| line.strip_prefix("*** Move to: "))
            .map(str::trim);
        if let Some(found) = found {
            if !found.is_empty() && !out.iter().any(|p| p == found) {
                out.push(found.to_owned());
            }
        }
    }
    if out.is_empty() {
        if let Some(path) = path {
            out.push(path.to_owned());
        }
    }
    out
}

/// The identifier under a 1-based line/column.
pub(super) fn identifier_at(text: &str, line: usize, column: usize) -> Option<String> {
    let chars: Vec<char> = text.lines().nth(line.checked_sub(1)?)?.chars().collect();
    let is_ident = |c: &char| c.is_alphanumeric() || *c == '_';
    let mut index = column.saturating_sub(1).min(chars.len().checked_sub(1)?);
    if !is_ident(&chars[index]) && index > 0 && is_ident(&chars[index - 1]) {
        index -= 1;
    }
    if !is_ident(&chars[index]) {
        return None;
    }
    let start = chars[..index]
        .iter()
        .rposition(|c| !is_ident(c))
        .map_or(0, |p| p + 1);
    let end = chars[index..]
        .iter()
        .position(|c| !is_ident(c))
        .map_or(chars.len(), |p| index + p);
    Some(chars[start..end].iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_paths_cover_both_formats() {
        let unified =
            "--- a/src/a.py\n+++ b/src/a.py\n@@ -1 +1 @@\n-x\n+y\n--- /dev/null\n+++ b/new.go\n";
        assert_eq!(patch_paths(unified, None), ["src/a.py", "new.go"]);
        let codex = "*** Begin Patch\n*** Update File: lib.rs\n*** Move to: lib2.rs\n*** Add File: b.ts\n*** End Patch\n";
        assert_eq!(patch_paths(codex, None), ["lib.rs", "lib2.rs", "b.ts"]);
        assert_eq!(patch_paths("@@ -1 +1 @@\n-a\n+b\n", Some("x.c")), ["x.c"]);
    }

    #[test]
    fn identifier_at_finds_the_word_under_the_cursor() {
        let text = "let total = compute_sum(a, b);\n";
        assert_eq!(identifier_at(text, 1, 13).as_deref(), Some("compute_sum"));
        assert_eq!(identifier_at(text, 1, 23).as_deref(), Some("compute_sum"));
        assert_eq!(identifier_at(text, 1, 24).as_deref(), Some("compute_sum"));
        assert_eq!(identifier_at(text, 1, 5).as_deref(), Some("total"));
        assert_eq!(identifier_at(text, 2, 1), None);
    }
}
