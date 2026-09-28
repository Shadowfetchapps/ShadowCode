//! Native command-line frontend. The same command service, permissions, event
//! journal, and engine are used by local runs and an already open desktop.
macro_rules! outln {
    ($($arg:tt)*) => { writeln!(std::io::stdout(), $($arg)*).context("Could not write terminal output")? };
}
macro_rules! out {
    ($($arg:tt)*) => { write!(std::io::stdout(), $($arg)*).context("Could not write terminal output")? };
}
macro_rules! errln {
    ($($arg:tt)*) => { writeln!(std::io::stderr(), $($arg)*).context("Could not write terminal status")? };
}
macro_rules! err {
    ($($arg:tt)*) => { write!(std::io::stderr(), $($arg)*).context("Could not write terminal status")? };
}
pub mod args;
pub(crate) mod backend;
mod registration;
mod tui;
mod watch;
use crate::{
    paths::{self, AppPaths},
    workspace::Workspace,
};
use anyhow::{bail, ensure, Context, Result};
pub use args::Options;
use args::{Background, Command, Mcp, Plugin, Remote, Run, TaskOptions, WorktreeArgs};
use backend::Backend;
use clap::Parser;
use serde_json::{json, Value};
use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};
pub struct Outcome {
    pub code: i32,
    pub value: Value,
    pub raw: Option<String>,
}
impl Outcome {
    fn value(value: Value) -> Self {
        Self {
            code: if value.get("ok") == Some(&Value::Bool(false)) || value["kind"] == "error" {
                1
            } else {
                0
            },
            value,
            raw: None,
        }
    }
}
impl Options {
    pub fn parse_args() -> Self {
        Self::parse()
    }
    pub fn desktop(&self) -> bool {
        matches!(self.command, None | Some(Command::Ui))
    }
    /// Stdout carries a JSON-RPC protocol (MCP or ACP), never a CLI result.
    pub fn mcp_stdio(&self) -> bool {
        matches!(
            self.command,
            Some(
                Command::Mcp {
                    action: Some(Mcp::Serve { .. })
                } | Command::Acp {
                    print_config: None,
                    ..
                }
            )
        )
    }
    pub fn events(&self) -> bool {
        match &self.command {
            Some(Command::Run(run)) => run.options.events,
            Some(Command::Command { task, .. } | Command::Skill { task, .. }) => task.events,
            _ => false,
        }
    }
    pub fn paths(&self) -> Result<AppPaths> {
        match &self.profile {
            Some(root) => AppPaths::isolated(&expand(root)?),
            None => AppPaths::discover(),
        }
    }
}
fn expand(path: &Path) -> Result<PathBuf> {
    let text = path.to_string_lossy();
    Ok(if text == "~" {
        PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
    } else if let Some(rest) = text.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(rest)
    } else {
        path.into()
    })
}
fn query(path: &str, pairs: &[(&str, &str)]) -> Result<String> {
    let mut url = reqwest::Url::parse(&format!("http://ipc.local{path}"))?;
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    Ok(format!("{}?{}", url.path(), url.query().unwrap_or("")))
}
fn unique(rows: &[Value], prefix: &str, label: &str) -> Result<String> {
    ensure!(!prefix.is_empty(), "Provide a {label} ID");
    let found: Vec<_> = rows
        .iter()
        .filter_map(|row| row["id"].as_str())
        .filter(|id| id.starts_with(prefix))
        .collect();
    ensure!(
        found.len() == 1,
        "Choose a unique {label} ID prefix; {} matches found",
        found.len()
    );
    Ok(found[0].into())
}
async fn resolve_id(backend: &Backend, kind: &str, prefix: &str) -> Result<String> {
    let result = backend
        .call(
            "GET",
            query("/api/resolve", &[("kind", kind), ("prefix", prefix)])?,
            Value::Null,
        )
        .await?;
    Ok(result["id"]
        .as_str()
        .context("Resolved identifier missing")?
        .into())
}
async fn session(backend: &Backend, id: Option<&str>, workspace: &Path) -> Result<String> {
    if let Some(id) = id {
        return resolve_id(backend, "session", id).await;
    }
    let rows = backend
        .call(
            "GET",
            query(
                "/api/sessions",
                &[
                    (
                        "workspace",
                        workspace.to_str().context("Workspace must be UTF-8")?,
                    ),
                    ("limit", "1"),
                ],
            )?,
            Value::Null,
        )
        .await?;
    rows["sessions"]
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["id"].as_str())
        .map(str::to_owned)
        .context("No conversation in this project; supply --session or run a task first")
}

async fn task(
    backend: &Backend,
    workspace: &Path,
    request: &str,
    options: &TaskOptions,
    json_output: bool,
    workflow: Option<(&str, &str)>,
) -> Result<Outcome> {
    ensure!(
        !options.detach || backend.persistent,
        "Detached tasks require an open desktop or `shadowcode serve`"
    );
    ensure!(
        !options.interactive || std::io::stdin().is_terminal(),
        "--interactive requires a terminal on stdin"
    );
    let sid = match &options.session {
        Some(id) => Some(session(backend, Some(id), workspace).await?),
        None => None,
    };
    let mut body = json!({"workspace":workspace,"session_id":sid,"model":options.model,"purpose":options.purpose,"queue":options.queue});
    let result = if let Some((name, args)) = workflow {
        body["name"] = json!(name);
        body["args"] = json!(args);
        backend.call("POST", "/api/commands/run", body).await?
    } else {
        body["task"] = json!(request);
        backend.call("POST", "/api/jobs", body).await?
    };
    let started = if workflow.is_some() {
        result["metadata"]["job"].clone()
    } else {
        result.clone()
    };
    if started["id"].is_null() {
        return Ok(Outcome::value(result));
    }
    if options.detach {
        return Ok(Outcome::value(started));
    }
    watch::job(backend, started, options, json_output, true).await
}
/// A completion script for `shell`, generated from the same definitions as
/// `--help`.
pub fn completions(shell: args::CompletionShell) -> Result<Vec<u8>> {
    use clap::CommandFactory;
    use clap_complete::{generate, Shell};
    let shell = match shell {
        args::CompletionShell::Bash => Shell::Bash,
        args::CompletionShell::Zsh => Shell::Zsh,
        args::CompletionShell::Fish => Shell::Fish,
    };
    let mut out = Vec::new();
    generate(shell, &mut Options::command(), "shadowcode", &mut out);
    Ok(out)
}
/// The `shadowcode(1)` manual page (roff), with the subcommands, the files
/// and the update-check switches that packagers look for.
pub fn manpage() -> Result<Vec<u8>> {
    use clap::CommandFactory;
    let command = Options::command()
        .name("shadowcode")
        .display_name("shadowcode")
        .long_about(
            "ShadowCode is a desktop coding agent for local GGUF models, vendor coding \
             subscriptions and OpenRouter. With no command it opens the desktop window; \
             the commands below run the same engine from a terminal.",
        );
    let man = clap_mangen::Man::new(command)
        .title("SHADOWCODE")
        .section("1")
        .source(format!("ShadowCode {}", crate::VERSION))
        .manual("User Commands");
    let mut out = Vec::new();
    man.render_title(&mut out)?;
    man.render_name_section(&mut out)?;
    man.render_synopsis_section(&mut out)?;
    man.render_description_section(&mut out)?;
    man.render_options_section(&mut out)?;
    // clap_mangen would point at one page per command (shadowcode-run(1));
    // only this page is installed, so list the commands here.
    out.extend_from_slice(b".SH COMMANDS\n");
    for command in Options::command().get_subcommands() {
        if command.is_hide_set() {
            continue;
        }
        let about = command
            .get_about()
            .map(|about| about.to_string())
            .unwrap_or_default();
        out.extend_from_slice(
            format!(
                ".TP\n\\fBshadowcode {}\\fR\n{}\n",
                roff_text(command.get_name()),
                roff_text(&about)
            )
            .as_bytes(),
        );
    }
    out.extend_from_slice(b".PP\nRun \\fBshadowcode\\fR \\fICOMMAND\\fR \\fB\\-\\-help\\fR for a command's options.\n");
    out.extend_from_slice(MANPAGE_EXTRA.as_bytes());
    man.render_version_section(&mut out)?;
    Ok(out)
}
/// Plain text as one roff line: escapes, and no leading control character.
fn roff_text(text: &str) -> String {
    let text = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('\\', "\\e")
        .replace('-', "\\-");
    if text.starts_with(['.', '\'']) {
        format!("\\&{text}")
    } else {
        text
    }
}
const MANPAGE_EXTRA: &str = r#".SH FILES
.TP
\fI~/.config/shadow-agent/config.yaml\fR
Settings. \fBupdates.check: false\fR turns off the daily update check.
.TP
\fI~/.config/shadow-agent/secrets.env\fR
API keys for HTTP providers (mode 600).
.TP
\fI~/.local/state/shadow-agent/\fR
Conversations, jobs and history (SQLite), and the update check's last answer.
.TP
\fI~/.local/share/shadow-agent/\fR
Webview storage, voice and code-intelligence models.
.TP
\fI/etc/shadowcode/policy.yaml\fR, \fI/usr/share/shadowcode/policy.yaml\fR
System policy. \fBupdates: {check: false}\fR turns the update check off for
every user; \fBupdates.message\fR replaces the update steps shown in
Settings > About.
.TP
\fI/usr/lib/shadowcode/\fR
The bundled llama.cpp runtime for local models.
.SH ENVIRONMENT
.TP
\fBXDG_CONFIG_HOME\fR, \fBXDG_DATA_HOME\fR, \fBXDG_STATE_HOME\fR
Base folders for the files above.
.SH PRIVACY
ShadowCode sends no telemetry. Once a day, unless turned off, it asks
api.github.com for the latest release of Shadowfetchapps/ShadowCode; the
request carries no version or identifier. Offline mode stops it.
.SH COPYRIGHT
Copyright 2026 Shadowfetch. ShadowCode was originally created by Shadowfetch.
Licensed under the Apache License, Version 2.0; see
\fI/usr/share/doc/shadow-code/copyright\fR.
.SH SEE ALSO
\fBgit\fR(1), \fBbwrap\fR(1)
.PP
Documentation: https://github.com/Shadowfetchapps/ShadowCode
"#;
/// The executable an editor or MCP client should launch, with the AppImage
/// flag when running from an AppImage.
fn launcher() -> Result<(PathBuf, Vec<String>)> {
    let current = std::env::current_exe()?;
    let appimage = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|image| {
            image.is_absolute()
                && image.is_file()
                && std::env::var_os("APPDIR")
                    .is_some_and(|dir| current.starts_with(PathBuf::from(dir)))
        });
    let mut args = Vec::new();
    if appimage.is_some() {
        args.push("--appimage-extract-and-run".into());
    }
    Ok((appimage.unwrap_or(current), args))
}
/// `shadowcode acp`: stdout carries JSON-RPC, so nothing else is printed there.
async fn acp(
    options: &Options,
    trust: bool,
    print_config: Option<args::AcpClient>,
    parent: Option<u32>,
) -> Result<i32> {
    ensure!(
        !options.json,
        "ACP stdout is reserved for JSON-RPC; omit --json"
    );
    if let Some(client) = print_config {
        let (executable, mut args) = launcher()?;
        if let Some(profile) = &options.profile {
            // The profile may not exist yet; the agent creates it on start.
            let profile = expand(profile)?;
            let profile = match profile.canonicalize() {
                Ok(path) => path,
                Err(_) => std::path::absolute(&profile)?,
            };
            args.extend([
                "--profile".into(),
                profile
                    .to_str()
                    .context("ACP profile path must be UTF-8")?
                    .to_owned(),
            ]);
        }
        args.push("acp".into());
        if trust {
            args.push("--trust".into());
        }
        outln!("{}", registration::acp(client, &executable, &args)?);
        return Ok(0);
    }
    let paths = options.paths()?;
    // Sessions name their own folders; this one only opens the engine when
    // no desktop is running, so fall back to a folder that exists.
    let workspace = [
        options.workspace.as_deref().map(expand).transpose()?,
        std::env::current_dir().ok(),
        paths.remembered_workspace(),
        std::env::var_os("HOME").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    .find_map(|path| Workspace::open(&path).ok())
    .context("No usable folder to open the engine in")?
    .path;
    let cancel = tokio_util::sync::CancellationToken::new();
    let signal = cancel.clone();
    let listener = tokio::spawn(async move {
        crate::lifecycle::interrupted(parent).await;
        signal.cancel();
    });
    let result = crate::acp_server::serve_io(
        paths,
        workspace,
        crate::acp_server::AcpOptions { trust },
        tokio::io::stdin(),
        tokio::io::stdout(),
        cancel,
    )
    .await;
    listener.abort();
    result?;
    Ok(0)
}
/// Terminal work runs before any GTK/Tauri initialization, including in AppImage.
pub async fn run(options: Options) -> Result<i32> {
    // Generated text only: no profile, project or engine is opened.
    match &options.command {
        Some(Command::Completions { shell }) => {
            std::io::stdout().write_all(&completions(*shell)?)?;
            return Ok(0);
        }
        Some(Command::Manpage) => {
            std::io::stdout().write_all(&manpage()?)?;
            return Ok(0);
        }
        _ => {}
    }
    let parent = crate::lifecycle::extraction_parent();
    if let Some(Command::Acp {
        trust,
        print_config,
    }) = &options.command
    {
        return acp(&options, *trust, *print_config, parent).await;
    }
    let workspace = Workspace::open(&expand(
        &options
            .workspace
            .clone()
            .unwrap_or(std::env::current_dir()?),
    )?)?
    .path;
    let paths = options.paths()?;
    if let Some(Command::Tui { session }) = &options.command {
        ensure!(
            !options.json,
            "The terminal interface does not support --json"
        );
        ensure!(
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            "The terminal interface requires a terminal on stdin and stdout"
        );
        let backend = Backend::open_tui(paths.clone(), workspace.clone(), parent).await?;
        let result = tui::run(&paths, workspace, session.as_deref(), parent).await;
        let closed = backend.close().await;
        result?;
        closed?;
        return Ok(0);
    }
    if let Some(Command::Mcp {
        action: Some(action @ (Mcp::Serve { .. } | Mcp::Register { .. })),
    }) = &options.command
    {
        let (allow_write, allow_approvals) = match action {
            Mcp::Serve {
                allow_write,
                allow_approvals,
                ..
            }
            | Mcp::Register {
                allow_write,
                allow_approvals,
                ..
            } => (*allow_write, *allow_approvals),
            _ => unreachable!(),
        };
        if let Mcp::Register {
            client,
            url,
            token_env,
            ..
        } = action
        {
            ensure!(
                !options.json || *client != args::McpClient::Codex,
                "Codex registration is TOML; omit --json"
            );
            if let Some(url) = url {
                outln!(
                    "{}",
                    registration::http(
                        *client,
                        url,
                        token_env
                            .as_deref()
                            .context("HTTP registration requires --token-env")?
                    )?
                );
                return Ok(0);
            }
            let (executable, mut args) = launcher()?;
            args.extend([
                "--workspace".to_owned(),
                workspace
                    .to_str()
                    .context("MCP project path must be UTF-8")?
                    .to_owned(),
            ]);
            if let Some(profile) = &options.profile {
                args.extend([
                    "--profile".into(),
                    expand(profile)?
                        .canonicalize()?
                        .to_str()
                        .context("MCP profile path must be UTF-8")?
                        .to_owned(),
                ]);
            }
            args.extend(["mcp".into(), "serve".into()]);
            if allow_write {
                args.push("--allow-write".into());
            }
            if allow_approvals {
                args.push("--allow-approvals".into());
            }
            outln!("{}", registration::stdio(*client, &executable, &args)?);
            return Ok(0);
        }
        ensure!(
            !options.json,
            "MCP stdout is reserved for JSON-RPC; omit --json"
        );
        if let Mcp::Serve {
            http: Some(address),
            token_env,
            ..
        } = action
        {
            let name = token_env.as_deref().context("HTTP requires --token-env")?;
            ensure!(
                crate::config::valid_secret_name(name),
                "Invalid bearer-secret reference"
            );
            let token = crate::config::secret(&paths, name)?
                .context("MCP HTTP bearer secret is not set")?;
            crate::mcp::server::http::validate(*address, &token)?;
            let socket = tokio::net::TcpListener::bind(address)
                .await
                .context("Could not bind MCP HTTP listener")?;
            let address = socket.local_addr()?;
            let cancel = tokio_util::sync::CancellationToken::new();
            let _cancel_on_drop = cancel.clone().drop_guard();
            let signal = cancel.clone();
            let listener = tokio::spawn(async move {
                crate::lifecycle::interrupted(parent).await;
                signal.cancel();
            });
            let (ready, ready_rx) = tokio::sync::oneshot::channel();
            let worker = tokio::spawn(crate::mcp::server::http::serve(
                paths,
                workspace,
                crate::mcp::server::Access {
                    allow_write,
                    allow_approvals,
                },
                socket,
                token,
                cancel.clone(),
                Some(ready),
            ));
            let announced = if ready_rx.await.is_ok() {
                writeln!(std::io::stderr(), "MCP HTTP listening on http://{address}/mcp; bearer authentication required; {}", if allow_write { "project changes enabled" } else { "read-only" })
                    .context("Could not write gateway startup status")
            } else {
                Ok(())
            };
            if announced.is_err() {
                cancel.cancel();
            }
            let result = worker.await.context("MCP HTTP gateway worker failed");
            listener.abort();
            announced?;
            result??;
            return Ok(0);
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let signal = cancel.clone();
        let listener = tokio::spawn(async move {
            crate::lifecycle::interrupted(parent).await;
            signal.cancel();
        });
        let result = crate::mcp::server::serve_io(
            paths,
            workspace,
            crate::mcp::server::Access {
                allow_write,
                allow_approvals,
            },
            tokio::io::stdin(),
            tokio::io::stdout(),
            cancel,
        )
        .await;
        listener.abort();
        result?;
        return Ok(0);
    }
    let serving = matches!(options.command, Some(Command::Serve { .. }));
    let events = options.events();
    let backend = Backend::open(paths, workspace.clone(), serving, parent).await?;
    let result = execute(&backend, &workspace, &options).await;
    let closed = backend.close().await;
    let outcome = match (result, closed) {
        (Ok(outcome), Ok(())) => outcome,
        (Err(error), Ok(())) => return Err(error),
        (result, Err(error)) => {
            return Err(error.context(format!(
                "Command result: {}",
                if result.is_ok() {
                    "completed"
                } else {
                    "failed"
                }
            )))
        }
    };
    if events {
        outln!(
            "{}",
            serde_json::to_string(
                &json!({"type":"result","exit_code":outcome.code,"result":outcome.value})
            )?
        );
    } else if options.json {
        outln!("{}", serde_json::to_string_pretty(&outcome.value)?);
    } else if let Some(raw) = outcome.raw {
        // Redirected exports are exact bytes. Interactive terminal rendering
        // and all human command output still remove terminal control codes.
        if matches!(options.command, Some(Command::Export { .. }))
            && !std::io::stdout().is_terminal()
        {
            std::io::stdout().write_all(raw.as_bytes())?;
        } else {
            std::io::stdout().write_all(watch::plain(&raw).as_bytes())?;
        }
        std::io::stdout().flush()?;
    } else {
        display(&outcome.value)?;
    }
    Ok(outcome.code)
}
/// Where `shadowcode serve --remote` listens, and what that means.
fn remote_banner(status: &Value, address: std::net::SocketAddr) -> String {
    let mut text = format!(
        "Remote access: {}",
        status["url"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("http://{address}"))
    );
    let tailnet = status["addresses"].as_array().is_some_and(|all| {
        all.iter().any(|a| {
            a["kind"] == "tailscale" && a["address"].as_str() == Some(&address.ip().to_string())
        })
    });
    if address.ip().is_loopback() {
        text.push_str(
            "\nOnly this computer can connect. For a phone, use `tailscale serve` (see docs/REMOTE.md) or choose another address with --remote-address.",
        );
    } else if tailnet {
        text.push_str("\nListening on your Tailscale address: devices on your tailnet can connect, encrypted by Tailscale.");
    } else {
        text.push_str(
            "\nWarning: plain HTTP is not encrypted. Anyone on this network can read the traffic; prefer Tailscale (docs/REMOTE.md).",
        );
    }
    if status["allow_terminals"] == true {
        text.push_str("\nTerminals are allowed over remote access.");
    }
    text
}

/// A pairing link with its QR code for a terminal.
fn pairing_text(pairing: &Value) -> Result<String> {
    let link = pairing["link"].as_str().context("Pairing link missing")?;
    Ok(format!(
        "Scan to pair a phone, or open this link on the device (works once, for {} minutes):\n{}\n{link}\nAnyone with this link can control ShadowCode until you unpair the device.",
        pairing["expires_in"].as_u64().unwrap_or(600) / 60,
        crate::remote::qr_terminal(link)?
    ))
}

fn remote_status_text(status: &Value) -> String {
    let mut text = if status["running"] == true {
        format!(
            "Remote access is on: {}\n",
            status["url"].as_str().unwrap_or("")
        )
    } else if status["enabled"] == true {
        format!(
            "Remote access is on but not running: {}\n",
            status["error"].as_str().unwrap_or("the engine is not open")
        )
    } else {
        "Remote access is off. Turn it on in Settings › Remote access, or run `shadowcode serve --remote`.\n".to_owned()
    };
    text.push_str(&format!(
        "Terminals over remote access: {}\n",
        if status["allow_terminals"] == true {
            "allowed"
        } else {
            "off"
        }
    ));
    let devices = status["devices"].as_array().cloned().unwrap_or_default();
    if devices.is_empty() {
        text.push_str("No paired devices.\n");
    }
    for device in devices {
        text.push_str(&format!(
            "{}  {}\n",
            device["id"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(8)
                .collect::<String>(),
            watch::plain(device["name"].as_str().unwrap_or(""))
        ));
    }
    text
}

fn display(value: &Value) -> Result<()> {
    if let Some(headline) = value["headline"].as_str() {
        outln!("{}", watch::plain(headline));
        if let Some(body) = value["body"].as_str().filter(|body| !body.is_empty()) {
            outln!("{}", watch::plain(body));
        }
        if let Some(diff) = value["diff"].as_str().filter(|diff| !diff.is_empty()) {
            outln!("{}", watch::plain(diff));
        }
        for item in value["items"].as_array().into_iter().flatten() {
            outln!(
                "{}: {}",
                watch::plain(item["label"].as_str().unwrap_or("")),
                watch::plain(item["value"].as_str().unwrap_or(""))
            );
        }
        if let Some(id) = value["metadata"]["session_id"].as_str() {
            outln!("Conversation: {id}");
        }
        if !value["metadata"]["panel"].is_null()
            || value["kind"] == "overlay"
            || !value["metadata"]["action"].is_null()
        {
            outln!(
                "Desktop action: {}",
                watch::plain(&value["metadata"].to_string())
            );
        }
    } else if value["project_hash"].is_string() && value["text"].is_string() {
        outln!("{}", watch::plain(value["text"].as_str().unwrap_or("")));
    } else if let Some(checks) = value["checks"].as_array() {
        outln!("Native diagnostics");
        for check in checks {
            outln!(
                "[{}] {}: {}",
                watch::plain(check["status"].as_str().unwrap_or("")),
                watch::plain(check["label"].as_str().unwrap_or("")),
                watch::plain(check["detail"].as_str().unwrap_or(""))
            );
            if let Some(fix) = check["fix"].as_str().filter(|v| !v.is_empty()) {
                outln!("  {}", watch::plain(fix));
            }
        }
    } else if value["project_map"].is_object() && value["text"].is_string() {
        outln!("{}", watch::plain(value["text"].as_str().unwrap_or("")));
    } else if value["log"].is_string() && value["diff"].is_object() {
        outln!(
            "{}\nWorking tree:\n{}\nStaged:\n{}\n{}",
            watch::plain(value["log"].as_str().unwrap_or("")),
            watch::plain(value["diff"]["diff"].as_str().unwrap_or("")),
            watch::plain(value["diff"]["staged"].as_str().unwrap_or("")),
            watch::plain(value["note"].as_str().unwrap_or(""))
        );
    } else if let Some(summary) = value["summary"].as_str() {
        outln!(
            "{}\n{}",
            watch::plain(summary),
            watch::plain(&format!(
                "{} · conversation {} · job {}",
                value["status"].as_str().unwrap_or(""),
                value["session_id"].as_str().unwrap_or(""),
                value["id"].as_str().unwrap_or("")
            ))
        );
    } else {
        outln!("{}", watch::plain(&serde_json::to_string_pretty(value)?));
    }
    Ok(())
}
async fn execute(backend: &Backend, workspace: &Path, options: &Options) -> Result<Outcome> {
    let command = options
        .command
        .as_ref()
        .context("No CLI command selected")?;
    let value = match command {
        Command::Ui => bail!("Desktop startup must use the native window"),
        Command::Tui { .. } => unreachable!("Terminal UI handled before CLI dispatch"),
        Command::Acp { .. } => unreachable!("ACP handled before CLI dispatch"),
        Command::Completions { .. } | Command::Manpage => {
            unreachable!("Generated text is printed before CLI dispatch")
        }
        Command::Sqlite {path,sql,params,limit,timeout_ms}=>backend.call("POST","/api/sqlite",json!({"path":path,"sql":sql,"params":serde_json::from_str::<Value>(params).context("--params must be a JSON array")?,"limit":limit,"timeout_ms":timeout_ms})).await?,
        Command::Memory {note,task,replace,expected_hash}=>backend.call("POST","/api/memory",json!({"action":if *replace{"replace"}else if note.is_some(){"append"}else{"read"},"scope":if task.is_some(){"task"}else{"project"},"task_id":task,"note":note,"expected_hash":expected_hash})).await?,
        Command::Doctor { test_model } => {
            backend
                .call(
                    "GET",
                    if *test_model {
                        "/api/doctor?test_model=true"
                    } else {
                        "/api/doctor"
                    },
                    Value::Null,
                )
                .await?
        }
        Command::Understand { save } => {
            backend
                .call("POST", "/api/workspace/understand", json!({"save":save}))
                .await?
        }
        Command::Why { path, count } => {
            backend
                .call(
                    "GET",
                    query(
                        "/api/workspace/why",
                        &[
                            ("path", path.as_deref().unwrap_or("")),
                            ("count", &count.to_string()),
                        ],
                    )?,
                    Value::Null,
                )
                .await?
        }
        Command::Serve {
            remote,
            remote_address,
        } => {
            ensure!(
                backend.service().is_some(),
                "This profile already has an engine"
            );
            errln!("ShadowCode {} · serving {}\nPress Ctrl-C to stop managed work and close the engine.",crate::VERSION,workspace.display());
            // A headless server runs scheduled automations like the desktop.
            backend.service().context("Native service unavailable")?.engine.start_automations();
            if *remote {
                let service = backend.service().context("Native service unavailable")?;
                let manager = service.remote();
                let address = match manager.address() {
                    // Remote access is switched on in Settings and already
                    // runs on the saved address.
                    Some(address) if remote_address.is_none() => address,
                    _ => manager.start(service, *remote_address)?,
                };
                errln!("{}", remote_banner(&manager.status(), address));
                let pairing = manager.pair(None)?;
                errln!("{}", pairing_text(&pairing)?);
            }
            let guardian_service=backend.service().context("Native service unavailable")?.clone();
            let guardian=tokio::spawn(async move {
                loop {
                    let cfg=crate::config::Config::load(guardian_service.engine.paths(),None)
                        .map(|c| crate::guardian::from_config_value(&c.guardian)).unwrap_or_default();
                    if !cfg.enabled {tokio::time::sleep(std::time::Duration::from_secs(30)).await;continue;}
                    tokio::time::sleep(crate::guardian::interval(&cfg)).await;
                    // Service reloads the config after sleeping, so disabling
                    // Guardian takes effect before the next scheduled check.
                    let result=guardian_service.dispatch(crate::service::Request {
                        method:"POST".into(),path:"/api/guardian/run".into(),body:json!({})
                    }).await;
                    if let Err(error)=result {tracing::info!("Guardian: {error}");}
                }
            });
            watch::interrupted(backend.parent).await;
            guardian.abort();
            return Ok(Outcome::value(json!({"status":"stopped"})));
        }
        Command::Run(Run {
            task: request,
            options: task_options,
        }) => {
            return task(
                backend,
                workspace,
                request,
                task_options,
                options.json,
                None,
            )
            .await
        }
        Command::Command {
            name,
            args,
            task: task_options,
        } => {
            return task(
                backend,
                workspace,
                "",
                task_options,
                options.json,
                Some((name.as_deref().unwrap_or("help"), args)),
            )
            .await
        }
        Command::Skill {
            name,
            args,
            list,
            task: task_options,
        } => {
            if *list || name.is_none() {
                backend
                    .call("GET", "/api/workspace/skills", Value::Null)
                    .await?
            } else {
                return task(
                    backend,
                    workspace,
                    "",
                    task_options,
                    options.json,
                    Some(("skill", &format!("{} {args}", name.as_ref().unwrap()))),
                )
                .await;
            }
        }
        Command::Exec { command, timeout } => {
            let request = backend.call(
                "POST",
                "/api/workspace/exec",
                json!({"workspace":workspace,"command":command,"timeout":timeout}),
            );
            let value = tokio::select! {result=request=>result?,_=watch::interrupted(backend.parent)=>return Ok(Outcome{code:130,value:json!({"status":"cancelled","command":command}),raw:None})};
            if options.json {
                value
            } else {
                return Ok(Outcome {
                    code: if value["ok"] == true { 0 } else { 1 },
                    raw: Some(format!(
                        "{}{}\nExit: {}\n",
                        value["stdout"].as_str().unwrap_or(""),
                        value["stderr"].as_str().unwrap_or(""),
                        value["exit_code"]
                    )),
                    value,
                });
            }
        }
        Command::Plugin { action } => match action {
            None => backend.call("GET", "/api/plugins", Value::Null).await?,
            Some(Plugin::Remove { name, hash }) => backend.call("POST", "/api/plugins/remove", json!({"workspace":workspace,"name":name,"hash":hash})).await?,
            Some(Plugin::Inspect { name, file } | Plugin::Install { name, file, .. }) => {
                let mut body = json!({"workspace":workspace,"name":name});
                if let Some(path) = file {
                    use std::io::Read;
                    use std::os::unix::fs::OpenOptionsExt;
                    let input = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path).context("Cannot open plugin bundle")?;
                    ensure!(input.metadata()?.is_file(), "Plugin bundle must be a regular file");
                    let mut text = String::new();
                    input.take((crate::plugins::MAX_BYTES + 1) as u64).read_to_string(&mut text)?;
                    ensure!(text.len() <= crate::plugins::MAX_BYTES, "Plugin bundle exceeds 256 KB");
                    body["bundle"] = serde_json::from_str(&text).context("Invalid plugin JSON")?;
                }
                let path = if let Some(Plugin::Install { hash, .. }) = action {
                    body["hash"] = json!(hash); "/api/plugins/install"
                } else { "/api/plugins/preview" };
                backend.call("POST", path, body).await?
            }
        },
        Command::Hooks {
            enable,
            disable,
            hash,
        } => {
            if let Some(path) = enable.as_ref().or(disable.as_ref()) {
                backend.call("POST","/api/hooks/activation",json!({"workspace":workspace,"path":path,"hash":hash.as_deref().unwrap_or(""),"enabled":enable.is_some()})).await?
            } else {
                backend.call("GET", "/api/hooks", Value::Null).await?
            }
        }
        Command::Mcp { action } => match action {
            Some(Mcp::Serve { .. } | Mcp::Register { .. }) => {
                unreachable!("MCP transport handled before CLI engine setup")
            }
            None => backend.call("GET", "/api/mcp/servers", Value::Null).await?,
            Some(Mcp::Add { definition, hash }) => {
                use std::io::Read;
                let file = std::fs::File::open(definition).context("Cannot open MCP definition")?;
                ensure!(
                    file.metadata()?.is_file(),
                    "MCP definition must be a regular file"
                );
                let mut text = String::new();
                file.take(32_001).read_to_string(&mut text)?;
                ensure!(text.len() <= 32_000, "MCP definition exceeds 32 KB");
                let definition: Value = serde_yaml_ng::from_str(&text)
                    .map_err(|_| anyhow::anyhow!("Invalid MCP JSON/YAML definition"))?;
                backend
                    .call(
                        "POST",
                        "/api/mcp/servers",
                        json!({"definition":definition,"hash":hash.as_deref().unwrap_or("")}),
                    )
                    .await?
            }
            Some(Mcp::Enable { server, hash }) => {
                backend
                    .call(
                        "POST",
                        "/api/mcp/activation",
                        json!({"workspace":workspace,"server":server,"hash":hash,"enabled":true}),
                    )
                    .await?
            }
            Some(Mcp::Disable { server }) => {
                backend
                    .call(
                        "POST",
                        "/api/mcp/activation",
                        json!({"workspace":workspace,"server":server,"enabled":false}),
                    )
                    .await?
            }
            Some(Mcp::Remove { server, hash }) => {
                backend
                    .call(
                        "POST",
                        "/api/mcp/servers/delete",
                        json!({"server":server,"hash":hash}),
                    )
                    .await?
            }
        },
        Command::Status => {
            let mut status = backend
                .call("GET", "/api/workspace/status", Value::Null)
                .await?;
            let jobs = backend.call("GET", "/api/jobs", Value::Null).await?;
            status["jobs"] = json!(jobs["jobs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|job| job["workspace"].as_str() == workspace.to_str()
                    && matches!(
                        job["status"].as_str(),
                        Some("queued" | "running" | "cancelling")
                    ))
                .collect::<Vec<_>>());
            status
        }
        Command::Trust => {
            backend
                .call("POST", "/api/projects/trust", json!({"path":workspace}))
                .await?
        }
        Command::Health { test_model } => {
            let mut health = backend.call("GET", "/api/health", Value::Null).await?;
            if *test_model {
                let result = backend.call("POST", "/api/models/test", json!({})).await?;
                health["ok"] = result["ok"].clone();
                health["provider_test"] = result;
            }
            health
        }
        Command::Models {
            selected,
            provider,
            endpoint,
            api_key_env,
            context_limit,
            no_detect,
        } => {
            if let Some(id) = selected {
                if provider.is_some() || endpoint.is_some() {
                    let registered=backend.call("POST","/api/models/register",json!({"name":id,"provider":provider,"endpoint":endpoint,"api_key_env":api_key_env,"context_limit":context_limit})).await?;
                    backend
                        .call(
                            "POST",
                            "/api/models/select",
                            json!({"id":registered["model"]["default"]}),
                        )
                        .await?
                } else {
                    backend
                        .call("POST", "/api/models/select", json!({"id":id}))
                        .await?
                }
            } else {
                ensure!(
                    provider.is_none()
                        && endpoint.is_none()
                        && api_key_env.is_none()
                        && context_limit.is_none(),
                    "Model registration options require --use"
                );
                backend
                    .call(
                        "GET",
                        if *no_detect {
                            "/api/models?detect=false"
                        } else {
                            "/api/models"
                        },
                        Value::Null,
                    )
                    .await?
            }
        }
        Command::Config { key, value } => {
            let config = backend.call("GET", "/api/config", Value::Null).await?;
            match (key, value) {
                (None, None) => config,
                (Some(key), None) => {
                    let mut selected = &config;
                    for part in key.split('.') {
                        selected = selected
                            .get(part)
                            .with_context(|| format!("Unknown setting: {key}"))?;
                    }
                    selected.clone()
                }
                (Some(key), Some(value)) => {
                    let parts: Vec<_> = key.split('.').collect();
                    ensure!(
                        parts.len() <= 8 && !parts.iter().any(|part| part.is_empty()),
                        "Invalid setting key"
                    );
                    let mut existing = &config;
                    for part in &parts {
                        existing = existing
                            .get(*part)
                            .with_context(|| format!("Unknown setting: {key}"))?;
                    }
                    let mut patch = serde_json::from_str(value).unwrap_or_else(|_| json!(value));
                    for part in parts.into_iter().rev() {
                        patch = json!({part:patch});
                    }
                    backend
                        .call("PUT", "/api/config", json!({"values":patch}))
                        .await?
                }
                _ => bail!("Provide a setting key before its value"),
            }
        }
        Command::Sessions {
            query: search,
            rename,
            delete,
        } => {
            if rename.is_some() || *delete {
                let id = resolve_id(backend,"session",search.as_deref().context("Provide a conversation ID prefix")?).await?;
                backend
                    .call(
                        if *delete { "DELETE" } else { "PATCH" },
                        format!("/api/sessions/{id}"),
                        json!({"title":rename}),
                    )
                    .await?
            } else {
                backend
                    .call(
                        "GET",
                        query(
                            "/api/sessions",
                            &[("q", search.as_deref().unwrap_or("")), ("limit", "1000")],
                        )?,
                        Value::Null,
                    )
                    .await?
            }
        }
        Command::Export {
            session: id,
            format,
            output,
        } => {
            let id = session(backend, id.as_deref(), workspace).await?;
            let export = backend
                .call(
                    "GET",
                    format!("/api/sessions/{id}/export?format={format}"),
                    Value::Null,
                )
                .await?;
            let content = export["content"].as_str().context("Export body missing")?;
            if let Some(path) = output {
                let path = expand(path)?;
                let path = if path.is_absolute() {
                    path
                } else {
                    std::env::current_dir()?.join(path)
                };
                paths::atomic_write(&path, content.as_bytes(), false)?;
                json!({"ok":true,"path":path,"bytes":content.len()})
            } else {
                return Ok(Outcome {
                    code: 0,
                    value: export.clone(),
                    raw: Some(content.into()),
                });
            }
        }
        Command::Worktree(options)=>{
            let WorktreeArgs {create,reference,inspect,remove,hash,recovery,restore,recovery_hash,review_return,return_changes,return_hash,review_changes,copy_changes,copy_hash,review_repair,repair,repair_hash} = options.as_ref();
            if let Some(id)=review_repair {backend.call("POST","/api/worktrees/review-repair",json!({"id":id})).await?}
            else if let Some(id)=repair {backend.call("POST","/api/worktrees/repair",json!({"id":id,"hash":repair_hash})).await?}
            else if *review_changes {backend.call("POST","/api/worktrees/review-changes",Value::Null).await?}
            else if *copy_changes {backend.call("POST","/api/worktrees/copy-changes",json!({"hash":copy_hash})).await?}
            else if let Some(id)=return_changes {
                let value=backend.call("POST","/api/worktrees/return",json!({"id":id,"hash":return_hash})).await?;
                return Ok(Outcome{code:if value["state"]=="merge_pending" {0}else{1},value,raw:None});
            }
            else if let Some(id)=review_return {backend.call("POST","/api/worktrees/review-return",json!({"id":id})).await?}
            else if let Some(id)=restore {backend.call("POST","/api/worktrees/restore",json!({"id":id,"hash":recovery_hash})).await?}
            else if let Some(id)=recovery {backend.call("POST","/api/worktrees/recovery",json!({"id":id})).await?}
            else if let Some(id)=remove {backend.call("POST","/api/worktrees/remove",json!({"id":id,"hash":hash})).await?}
            else if let Some(id)=inspect {backend.call("POST","/api/worktrees/inspect",json!({"id":id})).await?}
            else {backend.call(if *create {"POST"}else{"GET"},"/api/worktrees",json!({"reference":reference})).await?}
        },
        Command::Jobs {
            id,
            watch: watch_job,
            cancel,
            interactive,
        } => {
            if let Some(id) = id {
                let id = resolve_id(backend,"job",id).await?;
                let job = backend
                    .call(
                        if *cancel { "POST" } else { "GET" },
                        format!("/api/jobs/{id}{}", if *cancel { "/cancel" } else { "" }),
                        json!({}),
                    )
                    .await?;
                if *watch_job {
                    return watch::job(
                        backend,
                        job,
                        &TaskOptions {
                            interactive: *interactive,
                            approval: args::ApprovalMode::Wait,
                            ..Default::default()
                        },
                        options.json,
                        false,
                    )
                    .await;
                }
                job
            } else {
                ensure!(
                    !watch_job && !cancel,
                    "Supply a job ID with --watch or --cancel"
                );
                backend.call("GET", "/api/jobs", Value::Null).await?
            }
        }
        Command::Approvals {
            session: id,
            id: approval,
            decision,
        } => {
            let sid = match id {
                Some(id) => Some(session(backend, Some(id), workspace).await?),
                None => None,
            };
            if let (Some(id), Some(decision)) = (approval, decision) {
                ensure!(
                    id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                    "Invalid approval ID"
                );
                let sid = sid.context("Approval decisions require --session")?;
                backend
                    .call(
                        "POST",
                        format!("/api/approvals/{id}"),
                        json!({"session_id":sid,"decision":decision}),
                    )
                    .await?
            } else {
                ensure!(
                    approval.is_none(),
                    "Supply --decision approve or deny with --id"
                );
                backend
                    .call(
                        "GET",
                        query(
                            "/api/approvals",
                            &[("session_id", sid.as_deref().unwrap_or(""))],
                        )?,
                        Value::Null,
                    )
                    .await?
            }
        }
        Command::Goal {
            instruction,
            run,
            detach,
            interactive,
        } => {
            ensure!(
                !interactive || std::io::stdin().is_terminal(),
                "--interactive requires a terminal on stdin"
            );
            ensure!(
                !detach || backend.persistent,
                "Detached goals require an open desktop or shadowcode serve"
            );
            let goal = backend
                .call(
                    "POST",
                    "/api/goals",
                    json!({"workspace":workspace,"instruction":instruction,"run":run}),
                )
                .await?;
            if *run && !*detach {
                return watch::goal(
                    backend,
                    goal["id"].as_str().context("Goal ID missing")?,
                    *interactive,
                    options.json,
                )
                .await;
            }
            goal
        }
        Command::Goals {
            resume,
            pause,
            interactive,
        } => {
            ensure!(
                !interactive || std::io::stdin().is_terminal(),
                "--interactive requires a terminal on stdin"
            );
            let goals = backend.call("GET", "/api/goals", Value::Null).await?;
            if let Some(prefix) = resume.as_ref().or(pause.as_ref()) {
                let id = unique(
                    goals["goals"].as_array().context("Goals missing")?,
                    prefix,
                    "goal",
                )?;
                let value = backend
                    .call(
                        "POST",
                        format!(
                            "/api/goals/{id}/{}",
                            if pause.is_some() { "pause" } else { "run" }
                        ),
                        json!({}),
                    )
                    .await?;
                if resume.is_some() {
                    return watch::goal(backend, &id, *interactive, options.json).await;
                }
                value
            } else {
                goals
            }
        }
        Command::Background { action } => match action {
            Background::List => backend.call("GET", "/api/background", Value::Null).await?,
            Background::Start { name, command } => {
                ensure!(backend.persistent,"Background processes need an open desktop or `shadowcode serve`; their owner stays running to manage logs and cleanup");
                backend
                    .call(
                        "POST",
                        "/api/background",
                        json!({"name":name,"command":command}),
                    )
                    .await?
            }
            Background::Stop { id } | Background::Logs { id } => {
                let rows = backend.call("GET", "/api/background", Value::Null).await?;
                let id = unique(
                    rows["tasks"]
                        .as_array()
                        .context("Process history missing")?,
                    id,
                    "process",
                )?;
                let stop = matches!(action, Background::Stop { .. });
                backend
                    .call(
                        if stop { "POST" } else { "GET" },
                        format!("/api/background/{id}{}", if stop { "/stop" } else { "" }),
                        json!({}),
                    )
                    .await?
            }
        },
        Command::Remote { action } => match action {
            None | Some(Remote::Status) => {
                let status = backend.call("GET", "/api/remote", Value::Null).await?;
                if options.json {
                    status
                } else {
                    return Ok(Outcome {
                        code: 0,
                        raw: Some(remote_status_text(&status)),
                        value: status,
                    });
                }
            }
            Some(Remote::Pair { host }) => {
                let pairing = backend
                    .call(
                        "POST",
                        "/api/remote/pair",
                        json!({"host": host.as_deref().unwrap_or("")}),
                    )
                    .await?;
                if options.json {
                    pairing
                } else {
                    return Ok(Outcome {
                        code: 0,
                        raw: Some(pairing_text(&pairing)?),
                        value: pairing,
                    });
                }
            }
            Some(Remote::Revoke { id, all }) => {
                let body = match id {
                    Some(prefix) => {
                        let status = backend.call("GET", "/api/remote", Value::Null).await?;
                        let id = unique(
                            status["devices"].as_array().context("Device list missing")?,
                            prefix,
                            "device",
                        )?;
                        json!({"id": id})
                    }
                    None => json!({"all": all}),
                };
                backend
                    .call("POST", "/api/remote/devices/revoke", body)
                    .await?
            }
        },
        Command::Checkpoints {
            session: id,
            restore,
            undo,
        } => {
            let sid = session(backend, Some(id), workspace).await?;
            backend.call("POST","/api/commands/run",json!({"name":if *undo{"undo"}else if restore.is_some(){"rollback"}else{"checkpoints"},"args":restore.as_deref().unwrap_or(""),"session_id":sid})).await?
        }
    };
    Ok(Outcome::value(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_and_remote_arguments() {
        let parsed = Options::try_parse_from([
            "shadowcode",
            "serve",
            "--remote",
            "--remote-address",
            "100.64.0.2:7390",
        ])
        .unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Serve { remote: true, remote_address: Some(address) })
                if address.to_string() == "100.64.0.2:7390"
        ));
        // An address only makes sense with --remote.
        assert!(Options::try_parse_from([
            "shadowcode",
            "serve",
            "--remote-address",
            "127.0.0.1:1"
        ])
        .is_err());
        assert!(matches!(
            Options::try_parse_from(["shadowcode", "remote", "revoke", "--all"])
                .unwrap()
                .command,
            Some(Command::Remote {
                action: Some(Remote::Revoke {
                    id: None,
                    all: true
                })
            })
        ));
        assert!(Options::try_parse_from(["shadowcode", "remote", "revoke"]).is_err());
    }

    #[test]
    fn remote_text_explains_exposure() {
        let status = json!({"url": "http://192.168.1.5:7390", "addresses": [
            {"address": "100.64.0.2", "kind": "tailscale"},
        ]});
        let banner = |address: &str| remote_banner(&status, address.parse().unwrap());
        assert!(banner("192.168.1.5:7390").contains("not encrypted"));
        assert!(banner("100.64.0.2:7390").contains("encrypted by Tailscale"));
        assert!(banner("127.0.0.1:7390").contains("Only this computer"));
        let text = remote_status_text(&json!({"enabled": false, "devices": []}));
        assert!(text.contains("Remote access is off") && text.contains("No paired devices"));
        let pairing =
            pairing_text(&json!({"link": "http://127.0.0.1:7390/#pair=abc", "expires_in": 600}))
                .unwrap();
        assert!(pairing.contains("#pair=abc") && pairing.contains("10 minutes"));
    }
}
