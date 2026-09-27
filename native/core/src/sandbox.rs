//! Shell isolation for the native `exec` tool.
//!
//! With bubblewrap: system folders are read-only, the home directory is an
//! empty temporary one with only the configured toolchain folders
//! (`sandbox.home_binds`) mounted read-only, the project is writable, and the
//! network is off, on, or limited to `network.allow` through a proxy.
//! Credential folders (`~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.config`, ...) are
//! never mounted.
//!
//! Without bubblewrap: `sandbox.require` refuses the command; otherwise it
//! runs under Landlock file (and TCP) restrictions when the kernel has them,
//! and the user is warned once per conversation.
//!
//! Availability is probed before a user command runs; commands are never
//! replayed outside the sandbox after a runtime failure.
use crate::config::{Config, ShellNetwork};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
mod lsm;
#[cfg(target_os = "linux")]
mod netns;
pub mod proxy;

/// Home entries that toolchains commonly need, mounted read-only.
pub const DEFAULT_HOME_BINDS: &[&str] = &[
    ".cargo",
    ".rustup",
    ".nvm",
    ".npm",
    ".cache/pip",
    ".local/bin",
    ".gitconfig",
    ".pyenv",
    ".bun",
    ".deno",
];
/// Home entries that are never mounted, and are hidden when the project
/// itself contains them.
pub const DENIED_HOME: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".config",
    ".local/share",
    ".netrc",
    ".docker",
    ".kube",
    ".password-store",
    ".pki",
    ".azure",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    ".mozilla",
    ".var",
];
/// Credential files inside allowed toolchain folders; replaced by an empty file.
const MASKED_IN_BINDS: &[&str] = &[".cargo/credentials", ".cargo/credentials.toml"];
const SYSTEM_ROOTS: &[&str] = &[
    "/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt", "/nix",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SandboxConfig {
    /// Refuse shell commands when bubblewrap is unavailable.
    pub require: bool,
    /// Paths relative to the home directory mounted read-only in the sandbox.
    pub home_binds: Vec<String>,
    /// Apply Landlock when a command has to run without bubblewrap.
    pub landlock: bool,
}
impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            require: false,
            home_binds: DEFAULT_HOME_BINDS.iter().map(|s| (*s).to_owned()).collect(),
            landlock: true,
        }
    }
}
impl SandboxConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.home_binds.len() <= 64,
            "At most 64 sandbox.home_binds entries"
        );
        for entry in &self.home_binds {
            validate_home_entry(entry)
                .with_context(|| format!("Invalid sandbox.home_binds entry '{entry}'"))?;
        }
        Ok(())
    }
}

fn validate_home_entry(entry: &str) -> Result<()> {
    let path = Path::new(entry);
    ensure!(
        !entry.is_empty() && entry.len() <= 200 && !entry.contains('\0'),
        "Use a path relative to your home folder, for example .cargo"
    );
    ensure!(
        path.components().all(|c| matches!(c, Component::Normal(_))),
        "Use a path relative to your home folder without '..', '~' or a leading '/'"
    );
    ensure!(!entry.starts_with('~'), "Leave out '~/'");
    for denied in DENIED_HOME {
        let denied = Path::new(denied);
        ensure!(
            !(path.starts_with(denied) || denied.starts_with(path)),
            "{} holds credentials and is never mounted",
            denied.display()
        );
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SandboxMode {
    Off { reason: String },
    Bubblewrap,
}
#[derive(Clone, Debug)]
pub struct BubblewrapProfile {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub network: bool,
    pub scratch_dir: Option<PathBuf>,
    pub workspace_cow: bool,
}
#[derive(Clone, Debug, Default)]
pub struct ScratchSession {
    pub path: PathBuf,
}
static SCRATCH: LazyLock<Mutex<HashMap<PathBuf, tempfile::TempDir>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static COW_STATUS: Mutex<Option<String>> = Mutex::new(None);
static WARNED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let candidate = dir.join(name);
            let metadata = candidate.metadata().ok()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 == 0 {
                    return None;
                }
            }
            metadata.is_file().then_some(candidate)
        })
    })
}
pub fn detect() -> SandboxMode {
    match which("bwrap") {
        Some(_) => SandboxMode::Bubblewrap,
        None => SandboxMode::Off {
            reason: "bubblewrap is not installed".into(),
        },
    }
}

/// True the first time a conversation should see the "no bubblewrap" warning.
pub fn first_warning(session: &str) -> bool {
    WARNED
        .lock()
        .map(|mut seen| seen.insert(session.to_owned()))
        .unwrap_or(true)
}

/// Only directories created and retained by this process can be discarded.
pub fn create_scratch(base: &Path) -> Result<ScratchSession> {
    crate::paths::private_directory(base)?;
    let owned = tempfile::Builder::new()
        .prefix("shadowcode-scratch-")
        .tempdir_in(base)?;
    let path = owned.path().canonicalize()?;
    SCRATCH
        .lock()
        .map_err(|_| anyhow::anyhow!("Scratch lock poisoned"))?
        .insert(path.clone(), owned);
    Ok(ScratchSession { path })
}
pub fn discard_scratch(path: &Path) -> Result<Value> {
    let mut scratch = SCRATCH
        .lock()
        .map_err(|_| anyhow::anyhow!("Scratch lock poisoned"))?;
    let owned = scratch
        .remove(path)
        .context("Refusing to remove an unregistered scratch directory")?;
    owned.close()?;
    Ok(
        json!({"ok":true,"discarded":path,"note":"Removed this command's managed temporary directory."}),
    )
}
pub fn last_scratch() -> Option<PathBuf> {
    SCRATCH.lock().ok().and_then(|g| g.keys().next().cloned())
}
fn scratch_base() -> PathBuf {
    // SAFETY: geteuid cannot fail.
    std::env::temp_dir().join(format!("shadowcode-scratch-{}", unsafe { libc::geteuid() }))
}

fn probe(program: &Path, workspace: &Path) -> Result<()> {
    let mut child = Command::new(program)
        .args(profile_args(workspace, "true", false, None))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(
                status.success(),
                "bubblewrap is installed but its namespace/mount probe failed ({status})"
            );
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("bubblewrap availability probe timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn working_bwrap(workspace: &Path) -> Result<PathBuf> {
    let program = which("bwrap").context("bubblewrap is not installed")?;
    probe(&program, workspace)?;
    Ok(program)
}

/// Scratch availability is not evidence of workspace copy-on-write.
pub fn probe_workspace_cow() -> Value {
    let result = (|| -> Result<()> {
        let temp = tempfile::tempdir()?;
        working_bwrap(temp.path()).map(|_| ())
    })();
    let available = result.is_ok();
    let detail = match result {
        Ok(()) => "Bubblewrap works: shell commands see an empty home folder with read-only toolchains, and project writes are live. Copy-on-write and approve-before-keep are not implemented; rewind restores project files afterwards.".into(),
        Err(error) => format!("{error}. Workspace copy-on-write is unavailable."),
    };
    if let Ok(mut state) = COW_STATUS.lock() {
        *state = Some(detail.clone());
    }
    json!({"ok":false,"workspace_cow":false,"shell_available":available,
        "mode":if available {"scratch-only"} else {"unavailable"},
        "detail":detail,"kernel_proof":false})
}
pub fn cow_status_note() -> String {
    COW_STATUS
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| "Workspace writes are live; copy-on-write is unavailable".into())
}

/// Network inside the bubblewrap profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Net {
    Off,
    On,
    /// A private namespace whose only way out is the allow-list proxy.
    Proxy,
}

/// What the sandbox shows of the home folder.
#[derive(Clone, Debug, Default)]
pub struct HomeLayout {
    pub home: Option<PathBuf>,
    /// (real source, path inside the sandbox)
    pub binds: Vec<(PathBuf, PathBuf)>,
    /// Files replaced by an empty read-only file.
    pub masked_files: Vec<PathBuf>,
    /// Folders inside the project replaced by an empty folder.
    pub masked_dirs: Vec<PathBuf>,
    pub skipped: Vec<String>,
}

fn real(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

impl HomeLayout {
    pub fn current(entries: &[String], workspace: &Path) -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute() && h.parent().is_some());
        Self::build(home, entries, workspace)
    }
    pub fn build(home: Option<PathBuf>, entries: &[String], workspace: &Path) -> Self {
        let mut layout = Self {
            home: home.clone(),
            ..Self::default()
        };
        let Some(home) = home else {
            return layout;
        };
        let home_real = real(&home);
        let workspace = real(workspace);
        let denied: Vec<PathBuf> = DENIED_HOME.iter().map(|d| real(&home.join(d))).collect();
        for entry in entries {
            if validate_home_entry(entry).is_err() {
                layout.skipped.push(format!("{entry} (not allowed)"));
                continue;
            }
            let dest = home.join(entry);
            let Ok(source) = dest.canonicalize() else {
                continue;
            };
            // A symlink must not smuggle in a credential folder or the whole home.
            let unsafe_source = source == Path::new("/")
                || home_real.starts_with(&source)
                || denied
                    .iter()
                    .any(|d| source.starts_with(d) || d.starts_with(&source));
            if unsafe_source {
                layout
                    .skipped
                    .push(format!("{entry} (points at a protected folder)"));
                continue;
            }
            for masked in MASKED_IN_BINDS {
                if Path::new(masked).starts_with(entry) {
                    let path = home.join(masked);
                    if path.is_file() {
                        layout.masked_files.push(path);
                    }
                }
            }
            layout.binds.push((source, dest));
        }
        // A project that contains the home folder (or a credential folder)
        // must not expose it through its own writable mount.
        for (relative, resolved) in DENIED_HOME.iter().zip(&denied) {
            let path = home.join(relative);
            if !resolved.starts_with(&workspace) {
                continue;
            }
            match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_dir() => layout.masked_dirs.push(path),
                Ok(meta) if meta.is_file() => layout.masked_files.push(path),
                _ => {}
            }
        }
        layout
    }
}

/// Compatibility wrapper: the current home layout with the default binds.
pub fn profile_args(
    workspace: &Path,
    command: &str,
    allow_network: bool,
    scratch: Option<&Path>,
) -> Vec<String> {
    let binds: Vec<String> = DEFAULT_HOME_BINDS.iter().map(|s| (*s).to_owned()).collect();
    isolated_args(
        workspace,
        command,
        if allow_network { Net::On } else { Net::Off },
        scratch,
        &HomeLayout::current(&binds, workspace),
    )
}

pub fn isolated_args(
    workspace: &Path,
    command: &str,
    net: Net,
    scratch: Option<&Path>,
    layout: &HomeLayout,
) -> Vec<String> {
    let ws = real(workspace);
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let mut args: Vec<String> = [
        "--die-with-parent",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--new-session",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if net == Net::Off {
        args.push("--unshare-net".into());
    }
    for root in SYSTEM_ROOTS {
        if Path::new(root).exists() {
            args.extend(["--ro-bind".into(), (*root).into(), (*root).into()]);
        }
    }
    args.extend(
        [
            "--tmpfs", "/tmp", "--tmpfs", "/var/tmp", "--dev", "/dev", "--proc", "/proc",
        ]
        .map(str::to_owned),
    );
    // An empty home: other users' folders and every file in this user's
    // home are hidden, then the allowed toolchain folders come back read-only.
    if Path::new("/home").is_dir() {
        args.extend(["--tmpfs".into(), "/home".into()]);
    }
    if let Some(home) = &layout.home {
        if !SYSTEM_ROOTS.iter().any(|r| Path::new(r) == home) && home != Path::new("/") {
            args.extend(["--tmpfs".into(), s(home)]);
        }
        for (source, dest) in &layout.binds {
            args.extend(["--ro-bind".into(), s(source), s(dest)]);
        }
    }
    for file in layout.masked_files.iter().filter(|f| !f.starts_with(&ws)) {
        args.extend(["--ro-bind".into(), "/dev/null".into(), s(file)]);
    }
    // Bind the project after home, otherwise the home tmpfs hides it.
    args.extend(["--bind".into(), s(&ws), s(&ws)]);
    for dir in &layout.masked_dirs {
        args.extend(["--tmpfs".into(), s(dir)]);
    }
    for file in layout.masked_files.iter().filter(|f| f.starts_with(&ws)) {
        args.extend(["--ro-bind".into(), "/dev/null".into(), s(file)]);
    }
    if let Some(home) = &layout.home {
        args.extend(["--setenv".into(), "HOME".into(), s(home)]);
    }
    match net {
        Net::On => {
            // /etc/resolv.conf often links into /run, which is not mounted.
            if let Ok(resolv) = Path::new("/etc/resolv.conf").canonicalize() {
                if let Some(dir) = resolv.parent().filter(|d| d.starts_with("/run")) {
                    args.extend(["--ro-bind".into(), s(dir), s(dir)]);
                }
            }
        }
        Net::Proxy => {
            let url = format!("http://127.0.0.1:{}", proxy::PROXY_PORT);
            for name in [
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "http_proxy",
                "https_proxy",
                "ALL_PROXY",
                "all_proxy",
            ] {
                args.extend(["--setenv".into(), name.into(), url.clone()]);
            }
            for name in ["NO_PROXY", "no_proxy"] {
                args.extend(["--unsetenv".into(), name.into()]);
            }
        }
        Net::Off => {}
    }
    if let Some(scratch) = scratch {
        args.extend([
            "--bind".into(),
            s(scratch),
            "/shadowcode-scratch".into(),
            "--setenv".into(),
            "SHADOWCODE_SCRATCH".into(),
            "/shadowcode-scratch".into(),
        ]);
    }
    args.extend([
        "--chdir".into(),
        s(&ws),
        "--".into(),
        "/bin/sh".into(),
        "-c".into(),
        command.into(),
    ]);
    args
}

/// Work done in the forked child before exec (namespace or Landlock), and
/// the proxy served for it afterwards.
#[derive(Clone, Default)]
pub struct ChildSetup {
    #[cfg(target_os = "linux")]
    netns: Option<Arc<netns::Netns>>,
    #[cfg(target_os = "linux")]
    landlock: Option<Arc<std::os::fd::OwnedFd>>,
    gate: Option<Arc<proxy::Gate>>,
}
impl std::fmt::Debug for ChildSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("ChildSetup");
        #[cfg(target_os = "linux")]
        d.field("netns", &self.netns.is_some())
            .field("landlock", &self.landlock.is_some());
        d.field("proxy", &self.gate.is_some()).finish()
    }
}
/// Stops the allow-list proxy when the command's process is finished.
pub struct ProxyGuard(tokio::task::JoinHandle<()>);
impl Drop for ProxyGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl ChildSetup {
    /// # Safety
    /// Call only between fork and exec: it only makes system calls.
    pub unsafe fn enter(&self) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            if let Some(netns) = &self.netns {
                netns.enter()?;
            }
            if let Some(fd) = &self.landlock {
                lsm::restrict(fd)?;
            }
        }
        Ok(())
    }
    /// In the parent, right after the child started.
    pub fn after_spawn(&self) -> Result<Option<ProxyGuard>> {
        #[cfg(target_os = "linux")]
        if let (Some(netns), Some(gate)) = (&self.netns, &self.gate) {
            let listener = netns.receive()?;
            return Ok(Some(ProxyGuard(proxy::spawn(listener, gate.clone())?)));
        }
        Ok(None)
    }
}

/// The shell settings that apply to one command.
#[derive(Clone, Debug)]
pub struct ShellPolicy {
    pub network: ShellNetwork,
    pub allow: Vec<proxy::AllowEntry>,
    pub allow_text: Vec<String>,
    pub home_binds: Vec<String>,
    pub require: bool,
    pub landlock: bool,
}
impl ShellPolicy {
    pub fn from_config(config: &Config) -> Result<Self> {
        Ok(Self {
            network: config.shell_network(),
            allow: proxy::parse_list(&config.network.allow)?,
            allow_text: config.network.allow.clone(),
            home_binds: config.sandbox.home_binds.clone(),
            require: config.sandbox.require,
            landlock: config.sandbox.landlock,
        })
    }
}

/// A shell command ready to run.
#[derive(Debug)]
pub struct PreparedShell {
    pub program: String,
    pub args: Vec<String>,
    pub child: Option<ChildSetup>,
    pub scratch: Option<PathBuf>,
    pub gate: Option<Arc<proxy::Gate>>,
    /// Recorded with the tool result.
    pub note: Value,
    /// Shown once per conversation when isolation is weaker than expected.
    pub warning: Option<String>,
}

fn network_label(network: ShellNetwork) -> &'static str {
    match network {
        ShellNetwork::Off => "off",
        ShellNetwork::On => "on",
        ShellNetwork::Allowlist => "allowlist",
    }
}

pub fn prepare_shell(
    workspace: &Path,
    cwd: &Path,
    command: &str,
    policy: &ShellPolicy,
) -> Result<PreparedShell> {
    let layout = HomeLayout::current(&policy.home_binds, workspace);
    match working_bwrap(workspace) {
        Ok(program) => {
            let net = match policy.network {
                ShellNetwork::Off => Net::Off,
                ShellNetwork::On => Net::On,
                ShellNetwork::Allowlist => Net::Proxy,
            };
            let mut child = None;
            let mut gate = None;
            if net == Net::Proxy {
                #[cfg(target_os = "linux")]
                {
                    let shared = proxy::Gate::new(policy.allow.clone());
                    child = Some(ChildSetup {
                        netns: Some(Arc::new(netns::Netns::new()?)),
                        landlock: None,
                        gate: Some(shared.clone()),
                    });
                    gate = Some(shared);
                }
                #[cfg(not(target_os = "linux"))]
                bail!("The network allow-list needs Linux namespaces. The command did not run.");
            }
            let scratch = create_scratch(&scratch_base())?;
            let mut args = isolated_args(workspace, command, net, Some(&scratch.path), &layout);
            if let Some(index) = args.iter().position(|arg| arg == "--chdir") {
                args[index + 1] = cwd.to_string_lossy().into_owned();
            }
            let note = json!({
                "mode": "bubblewrap",
                "network": network_label(policy.network),
                "network_requested": network_label(policy.network),
                "network_enforcement": match net {
                    Net::Off => "private_namespace",
                    Net::On => "none",
                    Net::Proxy => "host_allowlist_proxy",
                },
                "allow": if net == Net::Proxy { json!(policy.allow_text) } else { Value::Null },
                "home": "empty temporary folder",
                "home_read_only": layout.binds.iter().map(|(_, d)| d.to_string_lossy()).collect::<Vec<_>>(),
                "home_skipped": layout.skipped,
                "scratch": scratch.path.to_string_lossy(),
                "workspace_cow": false,
                "note": "bubblewrap: read-only system folders, empty home with read-only toolchains, writable project, ephemeral scratch at SHADOWCODE_SCRATCH. Not a complete OS sandbox."
            });
            Ok(PreparedShell {
                program: program.to_string_lossy().into_owned(),
                args,
                child,
                scratch: Some(scratch.path),
                gate,
                note,
                warning: None,
            })
        }
        Err(error) => {
            if policy.require {
                bail!(
                    "The command did not run: 'Require sandbox' is on and bubblewrap is unavailable ({error:#}). Install bubblewrap (for example: sudo apt install bubblewrap), or turn off 'Require sandbox' in Settings > Permissions & network."
                );
            }
            if policy.network == ShellNetwork::Allowlist {
                bail!(
                    "The command did not run: the network allow-list needs bubblewrap, which is unavailable ({error:#}). Install bubblewrap, or change 'Shell commands and the network' in Settings > Permissions & network."
                );
            }
            let deny_tcp = policy.network == ShellNetwork::Off;
            #[cfg(target_os = "linux")]
            let landlock = if policy.landlock {
                let (read_only, writable) = landlock_paths(&layout, policy.network);
                lsm::ruleset(&lsm::Paths {
                    read_only,
                    writable,
                    workspace: &real(workspace),
                    deny_tcp,
                })
                .unwrap_or(None)
            } else {
                None
            };
            #[cfg(not(target_os = "linux"))]
            let landlock: Option<()> = None;
            let limited = landlock.is_some();
            // BestEffort supports older kernels too, but Landlock's TCP
            // rights start at ABI 4. Requested network=off is not proof that
            // any network restriction is enforced by the fallback.
            #[cfg(target_os = "linux")]
            let tcp_blocked = limited && deny_tcp && lsm::kernel_abi() >= 4;
            #[cfg(not(target_os = "linux"))]
            let tcp_blocked = false;
            let warning = if limited {
                format!(
                    "Shell commands are running without bubblewrap ({error:#}). Landlock limits them to this project, temporary folders and read-only system and toolchain folders{}; other isolation (process, network namespace) is missing. Install bubblewrap for the full sandbox, or turn on 'Require sandbox' to refuse commands instead.",
                    if tcp_blocked {
                        ", and blocks TCP connections (UDP remains unrestricted)"
                    } else if deny_tcp {
                        ", but this kernel cannot enforce the requested network restriction"
                    } else {
                        ""
                    }
                )
            } else {
                format!(
                    "Shell commands are running without a sandbox ({error:#}): they can read and change anything your user can. Install bubblewrap, or turn on 'Require sandbox' in Settings > Permissions & network to refuse commands instead."
                )
            };
            let note = json!({
                "mode": if limited { "landlock" } else { "none" },
                "reason": format!("{error:#}"),
                // Keep network as the historical requested-policy field.
                "network": network_label(policy.network),
                "network_requested": network_label(policy.network),
                "network_enforcement": if tcp_blocked { "tcp_only" } else { "none" },
                "note": if tcp_blocked {
                    "No bubblewrap: Landlock restricts supported file operations and blocks TCP. UDP and process isolation are not enforced."
                } else if limited {
                    "No bubblewrap: Landlock restricts supported file operations. Network and process isolation are not enforced."
                } else {
                    "No bubblewrap and no Landlock: the command runs as your user without isolation; only the approval policy applies."
                }
            });
            Ok(PreparedShell {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), command.into()],
                #[cfg(target_os = "linux")]
                child: landlock.map(|fd| ChildSetup {
                    netns: None,
                    landlock: Some(Arc::new(fd)),
                    gate: None,
                }),
                #[cfg(not(target_os = "linux"))]
                child: None,
                scratch: None,
                gate: None,
                note,
                warning: Some(warning),
            })
        }
    }
}

#[cfg(target_os = "linux")]
fn landlock_paths(layout: &HomeLayout, network: ShellNetwork) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut read_only: Vec<PathBuf> = SYSTEM_ROOTS
        .iter()
        .chain(&["/proc", "/sys"])
        .map(PathBuf::from)
        .collect();
    read_only.extend(layout.binds.iter().map(|(source, _)| source.clone()));
    if network != ShellNetwork::Off {
        if let Ok(resolv) = Path::new("/etc/resolv.conf").canonicalize() {
            if let Some(dir) = resolv.parent() {
                read_only.push(dir.to_path_buf());
            }
        }
    }
    let writable = ["/tmp", "/var/tmp", "/dev"].map(PathBuf::from).to_vec();
    (read_only, writable)
}

/// What shell isolation this computer and configuration provide.
pub fn status(config: &Config) -> Value {
    let installed = which("bwrap").is_some();
    let bwrap = (|| -> Result<()> {
        let temp = tempfile::tempdir()?;
        working_bwrap(temp.path()).map(|_| ())
    })();
    #[cfg(target_os = "linux")]
    let (landlock_abi, netns) = (lsm::kernel_abi(), netns::probe());
    #[cfg(not(target_os = "linux"))]
    let (landlock_abi, netns) = (0i64, Err::<(), _>(anyhow::anyhow!("Linux only")));
    let network = config.shell_network();
    let effective = match (&bwrap, config.sandbox.require, network) {
        (Ok(()), _, ShellNetwork::Allowlist) if netns.is_err() => "blocked",
        (Ok(()), _, _) => "bubblewrap",
        (Err(_), true, _) | (Err(_), _, ShellNetwork::Allowlist) => "blocked",
        (Err(_), false, _) if config.sandbox.landlock && landlock_abi > 0 => "landlock",
        _ => "none",
    };
    let layout = HomeLayout::current(&config.sandbox.home_binds, Path::new("/nonexistent"));
    json!({
        "effective": effective,
        "bubblewrap": {
            "installed": installed,
            "works": bwrap.is_ok(),
            "detail": match &bwrap { Ok(()) => "bubblewrap works".to_owned(), Err(e) => format!("{e:#}") },
        },
        "landlock_abi": landlock_abi,
        "network_namespace": {
            "available": netns.is_ok(),
            "detail": match &netns { Ok(()) => "Private network namespaces work".to_owned(), Err(e) => format!("{e:#}") },
        },
        "require": config.sandbox.require,
        "shell_network": network_label(network),
        "allow": config.network.allow,
        "home_read_only": layout.binds.iter().map(|(_, d)| d.to_string_lossy()).collect::<Vec<_>>(),
        "home_skipped": layout.skipped,
        "never_mounted": DENIED_HOME,
    })
}

pub fn doctor_checks() -> Vec<Value> {
    let report = probe_workspace_cow();
    #[cfg(target_os = "linux")]
    let abi = lsm::kernel_abi();
    #[cfg(not(target_os = "linux"))]
    let abi = 0;
    vec![
        json!({"id":"bubblewrap","status":if report["shell_available"]==true {"pass"} else {"info"},
        "title":"Shell sandbox (bubblewrap)","detail":report["detail"],
        "fix":"Install bubblewrap for the full shell sandbox. Without it, commands run under Landlock when the kernel supports it, or unsandboxed with a warning; turn on 'Require sandbox' to refuse them instead. A command is never replayed after a sandbox failure."}),
        json!({"id":"landlock","status":if abi > 0 {"pass"} else {"info"},
        "title":"Landlock fallback","detail":if abi > 0 {format!("Landlock ABI {abi} is available for commands that run without bubblewrap.")} else {"This kernel has no Landlock; without bubblewrap, commands run unsandboxed.".into()},
        "fix":"Landlock needs Linux 5.13 or later with the Landlock LSM enabled."}),
        json!({"id":"workspace-cow","status":"info","title":"Live workspace writes",
        "detail":"Shell writes affect the current workspace. Scratch storage is temporary, not copy-on-write. A checkpoint taken before each command lets rewind restore project files.",
        "fix":"Use an isolated Git worktree for changes you want to review before integrating."}),
    ]
}
pub fn doctor_check() -> Value {
    json!({"checks":doctor_checks()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn position(args: &[String], window: &[&str]) -> Option<usize> {
        args.windows(window.len()).position(|w| w == window)
    }
    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn cleanup_refuses_user_data_and_symlinks() {
        let base = tempfile::tempdir().unwrap();
        let user = base.path().join("project");
        fs::create_dir(&user).unwrap();
        fs::write(user.join("important"), "keep").unwrap();
        assert!(discard_scratch(&user).is_err());
        let scratch = create_scratch(base.path()).unwrap();
        fs::write(scratch.path.join("temp"), "x").unwrap();
        discard_scratch(&scratch.path).unwrap();
        assert!(!scratch.path.exists());
        assert!(user.join("important").exists());
    }

    /// A fake home with credentials and toolchains, and a project elsewhere.
    fn fake_home() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().canonicalize().unwrap().join("home").join("me");
        for dir in [
            ".ssh",
            ".aws",
            ".config/shadow-agent",
            ".cargo/bin",
            ".rustup",
            ".local/share/shadowcode",
        ] {
            fs::create_dir_all(home.join(dir)).unwrap();
        }
        fs::write(home.join(".ssh/id_ed25519"), "not a real key").unwrap();
        fs::write(home.join(".config/shadow-agent/secrets.env"), "X=not-real").unwrap();
        fs::write(home.join(".cargo/credentials.toml"), "token = \"not-real\"").unwrap();
        fs::write(home.join(".gitconfig"), "[user]\n").unwrap();
        let ws = root.path().canonicalize().unwrap().join("project");
        fs::create_dir_all(&ws).unwrap();
        (root, home, ws)
    }

    #[test]
    fn home_is_an_empty_tmpfs_with_read_only_toolchains_and_writable_workspace() {
        let (_root, home, ws) = fake_home();
        let binds: Vec<String> = DEFAULT_HOME_BINDS.iter().map(|s| (*s).into()).collect();
        let layout = HomeLayout::build(Some(home.clone()), &binds, &ws);
        let args = isolated_args(&ws, "echo hi", Net::Off, None, &layout);
        let (home_s, ws_s) = (text(&home), text(&ws));
        let cargo_s = text(&home.join(".cargo"));
        let tmpfs_home = position(&args, &["--tmpfs", &home_s]).expect("home is a tmpfs");
        let cargo =
            position(&args, &["--ro-bind", &cargo_s, &cargo_s]).expect("toolchain read-only");
        let gitconfig = text(&home.join(".gitconfig"));
        assert!(position(&args, &["--ro-bind", &gitconfig, &gitconfig]).is_some());
        let workspace = position(&args, &["--bind", &ws_s, &ws_s]).expect("workspace writable");
        assert!(tmpfs_home < cargo && cargo < workspace, "{args:?}");
        let credentials = text(&home.join(".cargo/credentials.toml"));
        assert!(
            position(&args, &["--ro-bind", "/dev/null", &credentials]).is_some(),
            "cargo credentials masked"
        );
        for secret in [".ssh", ".aws", ".config", ".local/share"] {
            let hidden = text(&home.join(secret));
            assert!(
                !args.iter().any(|a| a.starts_with(&hidden)),
                "{secret} must not be mounted: {args:?}"
            );
        }
        assert!(position(&args, &["--setenv", "HOME", &home_s]).is_some());
        assert!(args.iter().any(|a| a == "--unshare-net"));
        if Path::new("/home").is_dir() {
            assert!(position(&args, &["--tmpfs", "/home"]).is_some());
            assert!(position(&args, &["--ro-bind", "/home", "/home"]).is_none());
        }
    }

    #[test]
    fn network_modes_shape_the_profile() {
        let (_root, home, ws) = fake_home();
        let layout = HomeLayout::build(Some(home), &[], &ws);
        let on = isolated_args(&ws, "true", Net::On, None, &layout);
        assert!(!on.iter().any(|a| a == "--unshare-net"));
        let proxy = isolated_args(&ws, "true", Net::Proxy, None, &layout);
        // The private namespace is made before bubblewrap starts.
        assert!(!proxy.iter().any(|a| a == "--unshare-net"));
        let url = format!("http://127.0.0.1:{}", proxy::PROXY_PORT);
        assert!(position(&proxy, &["--setenv", "HTTPS_PROXY", &url]).is_some());
        assert!(position(&proxy, &["--setenv", "http_proxy", &url]).is_some());
        assert!(position(&proxy, &["--unsetenv", "NO_PROXY"]).is_some());
    }

    #[test]
    fn protected_home_entries_are_never_mounted() {
        for bad in [
            ".ssh",
            ".ssh/keys",
            ".config",
            ".config/git",
            ".local",
            ".local/share/shadowcode",
            ".aws",
            ".gnupg",
            "../x",
            "/etc",
            "~/.cargo",
            ".",
            "",
            "a/../.ssh",
        ] {
            assert!(validate_home_entry(bad).is_err(), "{bad}");
        }
        for good in [".cargo", ".local/bin", ".cache/pip", ".gitconfig", "go"] {
            validate_home_entry(good).unwrap();
        }
        let mut config = SandboxConfig::default();
        config.validate().unwrap();
        config.home_binds.push(".ssh".into());
        assert!(config.validate().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_smuggle_credentials_and_projects_hide_them() {
        let (_root, home, ws) = fake_home();
        std::os::unix::fs::symlink(home.join(".ssh"), home.join(".tools")).unwrap();
        std::os::unix::fs::symlink(&home, home.join(".homelink")).unwrap();
        let layout = HomeLayout::build(
            Some(home.clone()),
            &[".tools".into(), ".homelink".into()],
            &ws,
        );
        assert!(layout.binds.is_empty(), "{:?}", layout.binds);
        assert_eq!(layout.skipped.len(), 2);
        // A project that is the home folder hides the credential folders.
        let layout = HomeLayout::build(Some(home.clone()), &[], &home);
        let args = isolated_args(&home, "true", Net::Off, None, &layout);
        let home_s = text(&home);
        let bind = position(&args, &["--bind", &home_s, &home_s]).unwrap();
        let mask = position(&args, &["--tmpfs", &text(&home.join(".ssh"))]).expect(".ssh hidden");
        assert!(mask > bind);
        assert!(position(&args, &["--tmpfs", &text(&home.join(".config"))]).is_some());
    }

    #[test]
    fn scratch_probe_never_claims_copy_on_write() {
        let report = probe_workspace_cow();
        assert_eq!(report["ok"], false);
        assert_eq!(report["kernel_proof"], false);
    }

    #[test]
    fn missing_bubblewrap_reports_honest_fallback_and_fails_closed_when_required() {
        // PATH is process-global and tests share one process: run the mutation
        // in an isolated child so a concurrent test never observes an empty
        // PATH while resolving a subprocess (e.g. git).
        if std::env::var_os("SHADOWCODE_SANDBOX_FALLBACK_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sandbox::tests::missing_bubblewrap_reports_honest_fallback_and_fails_closed_when_required",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("SHADOWCODE_SANDBOX_FALLBACK_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let previous = std::env::var_os("PATH");
        // Empty PATH: bwrap cannot be resolved even if installed on the host.
        std::env::set_var("PATH", "");
        let mode = detect();
        let report = probe_workspace_cow();
        let checks = doctor_checks();
        let ws = tempfile::tempdir().unwrap();
        let mut policy = ShellPolicy {
            network: ShellNetwork::Off,
            allow: Vec::new(),
            allow_text: Vec::new(),
            home_binds: Vec::new(),
            require: true,
            landlock: true,
        };
        let required = prepare_shell(ws.path(), ws.path(), "true", &policy);
        policy.require = false;
        policy.network = ShellNetwork::Allowlist;
        let allowlist = prepare_shell(ws.path(), ws.path(), "true", &policy);
        policy.network = ShellNetwork::Off;
        let fallback = prepare_shell(ws.path(), ws.path(), "true", &policy);
        match previous {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
        assert!(matches!(
            mode,
            SandboxMode::Off { reason } if reason.contains("bubblewrap")
        ));
        assert_eq!(report["shell_available"], false);
        assert_eq!(report["mode"], "unavailable");
        assert_eq!(report["kernel_proof"], false);
        let bubble = checks
            .iter()
            .find(|c| c["id"] == "bubblewrap")
            .expect("doctor includes bubblewrap");
        assert_eq!(bubble["status"], "info");
        assert!(bubble["detail"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains("bubblewrap"));
        let required = format!("{:#}", required.unwrap_err());
        assert!(
            required.contains("did not run") && required.contains("Require sandbox"),
            "{required}"
        );
        let allowlist = format!("{:#}", allowlist.unwrap_err());
        assert!(
            allowlist.contains("did not run") && allowlist.contains("allow-list"),
            "{allowlist}"
        );
        let fallback = fallback.unwrap();
        assert_eq!(fallback.program, "/bin/sh");
        assert!(fallback
            .warning
            .as_deref()
            .unwrap_or("")
            .contains("without"));
        assert!(matches!(
            fallback.note["mode"].as_str(),
            Some("landlock" | "none")
        ));
    }

    #[test]
    fn the_no_sandbox_warning_is_shown_once_per_conversation() {
        let session = format!("warn-{}", crate::id());
        assert!(first_warning(&session));
        assert!(!first_warning(&session));
        assert!(first_warning(&format!("{session}-other")));
    }
}
