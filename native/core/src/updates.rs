//! The update notice and the facts Settings › About shows.
//!
//! At most once a day, and whenever the user presses **Check now**, ShadowCode
//! asks GitHub's releases API for the latest stable ShadowCode release and
//! compares it with this build. The request is a plain `GET` of one fixed URL
//! with a fixed `User-Agent` (GitHub requires one): no version, account, token,
//! cookie, query string or other identifier is sent, and nothing is downloaded
//! or installed. The notice says how to update this kind of installation.
//!
//! Off switches, strongest first:
//! - build time: `SHADOWCODE_UPDATE_CHECK=off` in the environment of
//!   `cargo build` (`default-off` only changes the default), with an optional
//!   `SHADOWCODE_UPDATE_MESSAGE` shown instead of update steps;
//! - package time: `updates.check: false` in `/etc/shadowcode/policy.yaml` or
//!   in `<prefix>/share/shadowcode/policy.yaml` next to the executable
//!   (`/usr/share/shadowcode/policy.yaml` for `/usr/bin/shadowcode`);
//! - the user: `updates.check` in config.yaml (Settings › About);
//! - network mode Offline pauses every check.
//!
//! A policy file that exists but cannot be read turns checks off: when in
//! doubt, ShadowCode stays quiet on the network.
use crate::{
    config::Config,
    paths::{atomic_write, AppPaths},
};
use anyhow::{bail, ensure, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    cmp::Ordering,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

pub const REPOSITORY_URL: &str = "https://github.com/Shadowfetchapps/ShadowCode";
pub const RELEASES_URL: &str = "https://github.com/Shadowfetchapps/ShadowCode/releases";
const DEFAULT_API: &str = "https://api.github.com/repos/Shadowfetchapps/ShadowCode/releases/latest";
/// Fixed; carries no version so every installation sends the same bytes.
pub const USER_AGENT: &str = "ShadowCode-update-check";
/// Automatic checks run at most this often.
pub const INTERVAL_SECS: f64 = 86_400.0;
/// "Check now" does not reach GitHub again within this many seconds.
pub const MANUAL_MIN_SECS: f64 = 30.0;
/// Administrator or distribution policy; wins over the packaged file.
pub const SYSTEM_POLICY: &str = "/etc/shadowcode/policy.yaml";
const STATE_FILE: &str = "update-check.json";
const MAX_RESPONSE_BYTES: usize = 2_000_000;
const MAX_POLICY_BYTES: u64 = 64 * 1024;
const MAX_MESSAGE_CHARS: usize = 300;
/// Files a release needs before the authenticated installer accepts it.
const SIGNATURE_FILES: [&str; 4] = [
    "SHA256SUMS",
    "RELEASE-MANIFEST.json",
    "RELEASE-AUTH",
    "RELEASE-AUTH.sig",
];

/// `updates` in config.yaml.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UpdatesConfig {
    /// Check GitHub for a newer release once a day. Unset (`null`) means the
    /// packaged default: on, unless the build or a policy file says
    /// `default-off`. Always written, so `shadowcode config updates.check`
    /// finds the key.
    pub check: Option<bool>,
}

/// A stable `MAJOR.MINOR.PATCH` version with an optional pre-release part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

impl Version {
    /// `0.33.1`, `v0.33.1`, `0.34.0-rc.1` or `0.34.0+build`. Components are
    /// at most nine digits without leading zeros.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let text = text.split_once('+').map_or(text, |(core, _)| core);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut parts = core.split('.');
        let mut number = || -> Option<u64> {
            let part = parts.next()?;
            let valid = !part.is_empty()
                && part.len() <= 9
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'));
            valid.then(|| part.parse().ok()).flatten()
        };
        let (major, minor, patch) = (number()?, number()?, number()?);
        if parts.next().is_some() {
            return None;
        }
        if let Some(pre) = pre {
            let valid = !pre.is_empty()
                && pre.len() <= 64
                && pre.split('.').all(|id| {
                    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                });
            if !valid {
                return None;
            }
        }
        Some(Self {
            major,
            minor,
            patch,
            pre: pre.map(str::to_owned),
        })
    }
    pub fn is_stable(&self) -> bool {
        self.pre.is_none()
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        Ok(())
    }
}

impl Ord for Version {
    /// Semantic-version precedence: numbers first; a pre-release sorts
    /// before its release; pre-release identifiers compare numerically
    /// when both are numbers, and numbers sort before words.
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => {
                    let mut left = left.split('.');
                    let mut right = right.split('.');
                    loop {
                        match (left.next(), right.next()) {
                            (None, None) => return Ordering::Equal,
                            (None, Some(_)) => return Ordering::Less,
                            (Some(_), None) => return Ordering::Greater,
                            (Some(a), Some(b)) => {
                                let order = match (a.parse::<u64>(), b.parse::<u64>()) {
                                    (Ok(a), Ok(b)) => a.cmp(&b),
                                    (Ok(_), Err(_)) => Ordering::Less,
                                    (Err(_), Ok(_)) => Ordering::Greater,
                                    (Err(_), Err(_)) => a.cmp(b),
                                };
                                if order != Ordering::Equal {
                                    return order;
                                }
                            }
                        }
                    }
                }
            })
    }
}
impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// True when `candidate` is a newer version than `current`. Unreadable
/// versions are never newer.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// The newest stable release, as far as the notice needs it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub tag: String,
    /// The release page (release notes and downloads), built from the tag.
    pub url: String,
    #[serde(default)]
    pub published_at: Option<String>,
    /// The release has the publisher signature files the installer needs.
    #[serde(default)]
    pub signed: bool,
}

/// Read one GitHub release object (`GET /repos/{repo}/releases/latest`).
/// Drafts, pre-releases and tags other than `vMAJOR.MINOR.PATCH` are
/// refused. Links are built from the tag, never taken from the response.
pub fn parse_release(value: &Value) -> Result<Release> {
    ensure!(value.is_object(), "GitHub returned an unexpected answer");
    ensure!(
        value["draft"] != Value::Bool(true) && value["prerelease"] != Value::Bool(true),
        "GitHub's latest release is not a stable release"
    );
    let tag = value["tag_name"]
        .as_str()
        .context("GitHub's latest release has no version tag")?;
    let version = tag
        .strip_prefix('v')
        .and_then(Version::parse)
        .filter(Version::is_stable)
        .filter(|version| format!("v{version}") == tag)
        .with_context(|| {
            format!(
                "GitHub's latest release has an unexpected tag ({})",
                tag.chars().take(40).collect::<String>()
            )
        })?;
    let assets: Vec<&str> = value["assets"]
        .as_array()
        .map(|assets| {
            assets
                .iter()
                .filter_map(|asset| asset["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let signed = SIGNATURE_FILES.iter().all(|name| assets.contains(name));
    let published_at = value["published_at"]
        .as_str()
        .filter(|text| {
            text.len() <= 40
                && text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b':' | b'.' | b'+'))
        })
        .map(str::to_owned);
    Ok(Release {
        version: version.to_string(),
        tag: tag.to_owned(),
        url: format!("{RELEASES_URL}/tag/{tag}"),
        published_at,
        signed,
    })
}

/// What the build and the system allow. Users cannot widen it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Policy {
    /// False: ShadowCode never checks (turned off at build or package time).
    pub allowed: bool,
    /// The user setting's value when config.yaml does not set it.
    pub default_on: bool,
    /// Shown instead of the usual update steps, e.g. "Updates come with
    /// Shadowfetch Linux system updates."
    pub message: Option<String>,
    /// Where the switch that turned checks off came from.
    pub source: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct PolicyFile {
    updates: PolicyUpdates,
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PolicyUpdates {
    check: Option<bool>,
    default: Option<bool>,
    message: Option<String>,
}

/// The effective policy of this executable.
pub fn policy() -> Policy {
    let packaged = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("share/shadowcode/policy.yaml")));
    let mut files: Vec<PathBuf> = packaged.into_iter().collect();
    files.push(PathBuf::from(SYSTEM_POLICY));
    policy_from(
        option_env!("SHADOWCODE_UPDATE_CHECK"),
        option_env!("SHADOWCODE_UPDATE_MESSAGE"),
        &files,
    )
}

/// Combine the build switches with the policy files; later files override
/// earlier ones. A build that turned checks off stays off.
pub fn policy_from(
    build_check: Option<&str>,
    build_message: Option<&str>,
    files: &[PathBuf],
) -> Policy {
    let mut policy = Policy {
        allowed: true,
        default_on: true,
        message: build_message.and_then(clean_message),
        source: None,
    };
    let mut build_off = false;
    match build_check
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("off" | "0" | "false" | "no" | "disabled") => build_off = true,
        Some("default-off") => policy.default_on = false,
        _ => {}
    }
    let mut file_check = None;
    let mut file_source = None;
    for file in files {
        match read_policy(file) {
            Ok(None) => {}
            Ok(Some(updates)) => {
                if let Some(check) = updates.check {
                    file_check = Some(check);
                    file_source = Some(file.display().to_string());
                }
                if let Some(default) = updates.default {
                    policy.default_on = default;
                }
                if let Some(message) = updates.message.as_deref().and_then(clean_message) {
                    policy.message = Some(message);
                }
            }
            Err(error) => {
                tracing::warn!("Update policy {} is unreadable: {error:#}", file.display());
                return Policy {
                    allowed: false,
                    default_on: false,
                    message: Some(format!(
                        "Update checks are off because {} could not be read.",
                        file.display()
                    )),
                    source: Some(file.display().to_string()),
                };
            }
        }
    }
    if build_off {
        policy.allowed = false;
        policy.source = Some("build".into());
    } else if file_check == Some(false) {
        policy.allowed = false;
        policy.source = file_source;
    }
    policy
}

fn read_policy(path: &Path) -> Result<Option<PolicyUpdates>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(file.metadata()?.is_file(), "not a regular file");
    let mut text = String::new();
    file.take(MAX_POLICY_BYTES + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() as u64 <= MAX_POLICY_BYTES,
        "larger than {MAX_POLICY_BYTES} bytes"
    );
    if text.trim().is_empty() {
        return Ok(Some(PolicyUpdates::default()));
    }
    let parsed: PolicyFile = serde_yaml_ng::from_str(&text)?;
    Ok(Some(parsed.updates))
}

/// One line of plain text, at most 300 characters.
fn clean_message(text: &str) -> Option<String> {
    let text = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then(|| text.chars().take(MAX_MESSAGE_CHARS).collect())
}

/// Whether automatic daily checks run, after the policy and the user setting.
pub fn automatic(config: &Config, policy: &Policy) -> bool {
    policy.allowed && config.updates.check.unwrap_or(policy.default_on)
}

/// How this copy of ShadowCode was installed; it decides the update steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallKind {
    Appimage,
    Deb,
    /// Under /usr or /opt without the bundler's package marker: a
    /// distribution's own build or another package format.
    System,
    /// Built from a source checkout (`target/debug` or `target/release`).
    Source,
    Unknown,
}

impl InstallKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Appimage => "AppImage",
            Self::Deb => "Debian package",
            Self::System => "System package",
            Self::Source => "Built from source",
            Self::Unknown => "Unknown",
        }
    }
}

static BUNDLE: OnceLock<InstallKind> = OnceLock::new();

/// The desktop shell passes the package format that Tauri's bundler wrote
/// into the executable (AppImage or Debian package).
pub fn set_bundle(kind: InstallKind) {
    let _ = BUNDLE.set(kind);
}

/// What [`detect_install`] looks at. The package manager's database is not
/// read: the bundler's marker already says "Debian package".
pub struct InstallProbe {
    pub bundle: Option<InstallKind>,
    pub appimage: Option<PathBuf>,
    pub appdir: Option<PathBuf>,
    pub exe: Option<PathBuf>,
}

pub fn install_kind() -> InstallKind {
    let var = |name| std::env::var_os(name).map(PathBuf::from);
    detect_install(&InstallProbe {
        bundle: BUNDLE.get().copied(),
        appimage: var("APPIMAGE"),
        appdir: var("APPDIR"),
        exe: std::env::current_exe().ok(),
    })
}

pub fn detect_install(probe: &InstallProbe) -> InstallKind {
    let exe = probe.exe.as_deref();
    if let (Some(image), Some(dir), Some(exe)) = (&probe.appimage, &probe.appdir, exe) {
        if image.is_absolute() && dir.is_absolute() && exe.starts_with(dir) {
            return InstallKind::Appimage;
        }
    }
    if matches!(probe.bundle, Some(InstallKind::Appimage | InstallKind::Deb)) {
        return probe.bundle.unwrap_or(InstallKind::Unknown);
    }
    let Some(exe) = exe else {
        return InstallKind::Unknown;
    };
    let names: Vec<_> = exe
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    if names
        .windows(2)
        .any(|pair| pair[0] == "target" && matches!(pair[1].as_ref(), "debug" | "release"))
    {
        return InstallKind::Source;
    }
    if (exe.starts_with("/usr") && !exe.starts_with("/usr/local")) || exe.starts_with("/opt") {
        return InstallKind::System;
    }
    InstallKind::Unknown
}

/// The commit this executable was built from, when the build recorded it
/// (`SHADOWCODE_COMMIT`, set by the release packaging).
pub fn commit() -> Option<&'static str> {
    option_env!("SHADOWCODE_COMMIT").filter(|commit| valid_commit(commit))
}

fn valid_commit(commit: &str) -> bool {
    let hash = commit.strip_suffix("-dirty").unwrap_or(commit);
    (7..=40).contains(&hash.len())
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Saved between runs in the profile's state folder.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct State {
    /// When ShadowCode last asked GitHub (seconds since the epoch).
    pub last_attempt: f64,
    /// When GitHub last answered with a release.
    pub last_success: f64,
    pub latest: Option<Release>,
    /// Why the last attempt failed.
    pub error: Option<String>,
    /// The version whose notice the user hid.
    pub dismissed: String,
}

fn state_path(paths: &AppPaths) -> PathBuf {
    paths.state.join(STATE_FILE)
}

pub fn load_state(paths: &AppPaths) -> State {
    let Ok(file) = fs::File::open(state_path(paths)) else {
        return State::default();
    };
    let mut bytes = Vec::new();
    if file.take(256 * 1024).read_to_end(&mut bytes).is_err() {
        return State::default();
    }
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn save_state(paths: &AppPaths, state: &State) -> Result<()> {
    atomic_write(
        &state_path(paths),
        &serde_json::to_vec_pretty(state)?,
        false,
    )
}

/// An automatic check is due: never checked, a day has passed, or the
/// clock moved back more than an hour since the last attempt.
pub fn due(state: &State, now: f64) -> bool {
    let never = !state.last_attempt.is_finite() || state.last_attempt <= 0.0;
    never || now - state.last_attempt >= INTERVAL_SECS || state.last_attempt - now > 3600.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// The window's daily check; silent when it cannot run.
    Automatic,
    /// Settings › About › Check now.
    Manual,
}

/// The releases API. `SHADOWCODE_UPDATE_API` exists for tests and must be
/// HTTPS or loopback.
pub fn api_url() -> String {
    std::env::var("SHADOWCODE_UPDATE_API")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| value.starts_with("https://") || crate::models::is_loopback_endpoint(value))
        .unwrap_or_else(|| DEFAULT_API.to_owned())
}

static CHECKING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Ask GitHub when the policy, the network mode and the schedule allow it,
/// and record the answer. An automatic check that is not allowed or not due
/// does nothing; a manual one that is not allowed fails with the reason.
pub async fn run_check(paths: &AppPaths, config: &Config, trigger: Trigger) -> Result<()> {
    let policy = policy();
    run_check_with(paths, config, &policy, trigger, &api_url()).await
}

pub async fn run_check_with(
    paths: &AppPaths,
    config: &Config,
    policy: &Policy,
    trigger: Trigger,
    url: &str,
) -> Result<()> {
    let manual = trigger == Trigger::Manual;
    if !policy.allowed {
        if manual {
            bail!(
                "{}",
                policy
                    .message
                    .clone()
                    .unwrap_or_else(|| "Update checks are turned off for this installation.".into())
            );
        }
        return Ok(());
    }
    if config.offline() {
        if manual {
            bail!("ShadowCode is in Offline mode. Choose Online or Web tools off in Settings › Permissions & network to check for updates.");
        }
        return Ok(());
    }
    if !manual && !automatic(config, policy) {
        return Ok(());
    }
    let _guard = CHECKING.lock().await;
    let mut state = load_state(paths);
    let now = crate::now();
    let recent = now >= state.last_attempt && now - state.last_attempt < MANUAL_MIN_SECS;
    if (manual && recent) || (!manual && !due(&state, now)) {
        return Ok(());
    }
    // Record the attempt first, so a hang or crash still counts toward the
    // once-a-day limit.
    state.last_attempt = now;
    save_state(paths, &state)?;
    match fetch_latest(url).await {
        Ok(release) => {
            state.latest = Some(release);
            state.last_success = crate::now();
            state.error = None;
        }
        Err(error) => {
            tracing::info!("Update check failed: {error:#}");
            state.error = Some(format!("{error:#}"));
        }
    }
    save_state(paths, &state)
}

/// `GET` the latest release. Redirects are followed only within
/// api.github.com over HTTPS.
pub async fn fetch_latest(url: &str) -> Result<Release> {
    let mut builder = reqwest::Client::builder();
    if crate::models::is_loopback_endpoint(url) {
        // Test servers on this computer; a proxy could not reach them.
        builder = builder.no_proxy();
    }
    let client = builder
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            if attempt.previous().len() < 3
                && url.scheme() == "https"
                && url.host_str() == Some("api.github.com")
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .user_agent(USER_AGENT)
        .build()?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Couldn't reach GitHub"))?;
    match response.status().as_u16() {
        200 => {}
        404 => bail!("GitHub lists no published release yet"),
        403 | 429 => bail!("GitHub asked ShadowCode to wait (rate limit)"),
        code => bail!("GitHub answered with HTTP {code}"),
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| anyhow::anyhow!("The answer from GitHub was cut off"))?;
        ensure!(
            bytes.len() + chunk.len() <= MAX_RESPONSE_BYTES,
            "GitHub's answer was too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    let value: Value =
        serde_json::from_slice(&bytes).context("GitHub's answer could not be read")?;
    parse_release(&value)
}

/// Hide the notice for `version` until a newer one appears.
pub fn dismiss(paths: &AppPaths, version: &str) -> Result<()> {
    let version = Version::parse(version).context("Unknown version")?;
    let mut state = load_state(paths);
    state.dismissed = version.to_string();
    save_state(paths, &state)
}

/// The steps to update this kind of installation to `release`.
pub fn next_step(install: InstallKind, policy: &Policy, release: Option<&Release>) -> Value {
    if let Some(message) = &policy.message {
        return json!({"text": message, "command": null, "link": null});
    }
    let tag = release.map_or("main", |release| release.tag.as_str());
    let version = release.map_or("VERSION", |release| release.version.as_str());
    let readme = |anchor: &str| format!("{REPOSITORY_URL}/blob/{tag}/README.md#{anchor}");
    match install {
        InstallKind::Appimage if release.is_some_and(|release| !release.signed) => json!({
            "text": "This release has no publisher signature files yet, so the installer will refuse it. Check again later.",
            "command": null,
            "link": null,
        }),
        InstallKind::Appimage => json!({
            "text": format!("Download ShadowCode_{version}_amd64.AppImage and its four signature files (SHA256SUMS, RELEASE-MANIFEST.json, RELEASE-AUTH, RELEASE-AUTH.sig) from the release page into one folder. Then run the installer from a trusted copy of ShadowCode's installer bundle. It checks the publisher signature before it runs anything and keeps your settings."),
            "command": format!("bash /path/to/trusted-bundle/scripts/install-appimage.sh ~/Downloads/ShadowCode_{version}_amd64.AppImage"),
            "link": readme("appimage-recommended"),
        }),
        InstallKind::Deb => json!({
            "text": "Update through your package manager or your distribution's software updates. If you installed the .deb from GitHub yourself, download the new .deb and its four signature files, verify them, then install it with apt.",
            "command": null,
            "link": readme("debian-package"),
        }),
        InstallKind::System => json!({
            "text": "Update ShadowCode through your package manager or your distribution's software updates.",
            "command": null,
            "link": null,
        }),
        InstallKind::Source => json!({
            "text": "Fetch the new tag and build it again.",
            "command": format!("git fetch --tags && git checkout {tag}"),
            "link": readme("build-from-source"),
        }),
        InstallKind::Unknown => json!({
            "text": "Download the new version from the release page and install it the way you installed this one.",
            "command": null,
            "link": null,
        }),
    }
}

/// `GET /api/updates`: the notice and its settings, without network access.
pub fn status(paths: &AppPaths, config: &Config, policy: &Policy, install: InstallKind) -> Value {
    let state = load_state(paths);
    let latest = state.latest.as_ref().filter(|_| policy.allowed);
    let available = latest.is_some_and(|release| is_newer(&release.version, crate::VERSION));
    let dismissed = available && latest.is_some_and(|release| release.version == state.dismissed);
    let when = |time: f64| (time > 0.0).then_some(time);
    json!({
        "current": crate::VERSION,
        "allowed": policy.allowed,
        "automatic": automatic(config, policy),
        "setting": config.updates.check,
        "default_on": policy.default_on,
        "offline": config.offline(),
        "policy_message": policy.message,
        "policy_source": policy.source,
        "install": {"kind": install, "label": install.label()},
        "last_checked_at": when(state.last_success),
        "last_attempt_at": when(state.last_attempt),
        "error": state.error.as_ref().filter(|_| policy.allowed),
        "latest": latest,
        "available": available,
        "dismissed": dismissed,
        "next_step": available.then(|| next_step(install, policy, latest)),
        "releases_url": RELEASES_URL,
    })
}

/// ShadowCode's own NOTICE (Apache-2.0 section 4(d)).
pub const NOTICE: &str = include_str!("../../../NOTICE");

/// `GET /api/about`.
pub fn about(paths: &AppPaths, config: &Config) -> Value {
    let install = install_kind();
    let tag = format!("v{}", crate::VERSION);
    let notices = std::env::current_exe().ok().and_then(|exe| {
        let dir = exe.parent()?.parent()?.join("share/doc/shadowcode/notices");
        dir.is_dir().then(|| dir.display().to_string())
    });
    json!({
        "name": "ShadowCode",
        "version": crate::VERSION,
        "commit": commit(),
        "install": {"kind": install, "label": install.label()},
        "license": {
            "spdx": "Apache-2.0",
            "name": "Apache License 2.0",
            "holder": "Shadowfetch",
            "notice": NOTICE,
            "third_party": notices,
        },
        "links": {
            "repository": REPOSITORY_URL,
            "release_notes": format!("{RELEASES_URL}/tag/{tag}"),
            "releases": RELEASES_URL,
            "license": format!("{REPOSITORY_URL}/blob/{tag}/LICENSE"),
            "notice": format!("{REPOSITORY_URL}/blob/{tag}/NOTICE"),
            "issues": format!("{REPOSITORY_URL}/issues"),
            "user_guide": format!("{REPOSITORY_URL}/blob/{tag}/docs/USER_GUIDE.md"),
        },
        "updates": status(paths, config, &policy(), install),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/updates")
            .join(name);
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn versions_compare_by_number_not_text() {
        let v = |text| Version::parse(text).unwrap();
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.33.10") > v("0.33.9"));
        assert_eq!(v("v0.33.1"), v("0.33.1"));
        assert_eq!(v("0.33.1+build.7"), v("0.33.1"));
        assert!(v("0.34.0") > v("0.34.0-rc.1"));
        assert!(v("0.34.0-rc.2") > v("0.34.0-rc.1"));
        assert!(v("0.34.0-rc.10") > v("0.34.0-rc.9"));
        assert!(v("0.34.0-beta") > v("0.34.0-alpha"));
        assert!(v("0.34.0-alpha.beta") > v("0.34.0-alpha.1"));
        assert!(v("0.34.0-alpha.1") > v("0.34.0-alpha"));
        assert_eq!(v("0.34.0-rc.1").to_string(), "0.34.0-rc.1");
        for bad in [
            "",
            "v",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "a.b.c",
            "1.2.-3",
            "1.2.3-",
            "1.2.3-a..b",
            "1.2.3-ä",
            "1234567890.0.0",
            " 1. 2.3",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        assert!(is_newer("0.34.0", "0.33.1"));
        assert!(!is_newer("0.33.1", "0.33.1"));
        assert!(!is_newer("0.33.0", "0.33.1"));
        assert!(!is_newer("garbage", "0.33.1"));
        assert!(!is_newer("0.34.0", "garbage"));
        assert!(is_newer("0.34.0", "0.34.0-dev"));
    }

    #[test]
    fn release_json_is_read_from_github_fixtures() {
        let release = parse_release(&fixture("latest-signed.json")).unwrap();
        assert_eq!(release.version, "0.34.0");
        assert_eq!(release.tag, "v0.34.0");
        // The link is built from the tag, not taken from html_url.
        assert_eq!(
            release.url,
            "https://github.com/Shadowfetchapps/ShadowCode/releases/tag/v0.34.0"
        );
        assert_eq!(
            release.published_at.as_deref(),
            Some("2026-10-02T09:15:00Z")
        );
        assert!(release.signed);

        let unsigned = parse_release(&fixture("latest-unsigned.json")).unwrap();
        assert_eq!(unsigned.version, "0.32.0");
        assert!(!unsigned.signed);

        for (name, reason) in [
            ("prerelease.json", "not a stable release"),
            ("draft.json", "not a stable release"),
            ("bad-tag.json", "unexpected tag"),
            ("no-tag.json", "no version tag"),
        ] {
            let error = parse_release(&fixture(name)).unwrap_err().to_string();
            assert!(error.contains(reason), "{name}: {error}");
        }
        assert!(parse_release(&json!([])).is_err());
        assert!(parse_release(&json!({"tag_name": "v0.34.0-rc.1"})).is_err());
        assert!(parse_release(&json!({"tag_name": "0.34.0"})).is_err());
        assert!(parse_release(&json!({"tag_name": "v00.34.0"})).is_err());
        // Odd published_at text is dropped rather than shown.
        let odd =
            parse_release(&json!({"tag_name": "v1.0.0", "published_at": "<script>"})).unwrap();
        assert_eq!(odd.published_at, None);
    }

    #[test]
    fn automatic_checks_run_at_most_once_a_day() {
        let now = 1_800_000_000.0;
        assert!(due(&State::default(), now));
        let at = |last_attempt| State {
            last_attempt,
            ..State::default()
        };
        assert!(!due(&at(now - 60.0), now));
        assert!(!due(&at(now - INTERVAL_SECS + 1.0), now));
        assert!(due(&at(now - INTERVAL_SECS), now));
        assert!(due(&at(now - 3.0 * INTERVAL_SECS), now));
        // A clock set back by more than an hour does not block checks forever.
        assert!(!due(&at(now + 1800.0), now));
        assert!(due(&at(now + 7200.0), now));
        assert!(due(&at(f64::NAN), now));
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn distributions_and_builds_can_turn_checks_off() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.yaml");
        let open = policy_from(None, None, std::slice::from_ref(&missing));
        assert_eq!(
            open,
            Policy {
                allowed: true,
                default_on: true,
                message: None,
                source: None
            }
        );

        let build = policy_from(
            Some("off"),
            Some("  Updates come with\nShadowfetch Linux.  "),
            &[],
        );
        assert!(!build.allowed);
        assert_eq!(build.source.as_deref(), Some("build"));
        assert_eq!(
            build.message.as_deref(),
            Some("Updates come with Shadowfetch Linux.")
        );
        let default_off = policy_from(Some("default-off"), None, &[]);
        assert!(default_off.allowed && !default_off.default_on);

        let packaged = write(
            dir.path(),
            "packaged.yaml",
            "updates:\n  check: false\n  message: Updated by Shadowfetch Linux Software Updates.\n",
        );
        let off = policy_from(None, None, std::slice::from_ref(&packaged));
        assert!(!off.allowed);
        assert_eq!(off.source.as_deref(), Some(packaged.to_str().unwrap()));
        assert_eq!(
            off.message.as_deref(),
            Some("Updated by Shadowfetch Linux Software Updates.")
        );

        // /etc comes last and wins over the packaged default.
        let etc_on = write(dir.path(), "etc.yaml", "updates:\n  check: true\n");
        assert!(policy_from(None, None, &[packaged.clone(), etc_on.clone()]).allowed);
        // A build that turned checks off stays off whatever the files say.
        assert!(!policy_from(Some("off"), None, &[etc_on]).allowed);

        let default = write(dir.path(), "default.yaml", "updates:\n  default: false\n");
        let policy = policy_from(None, None, &[default]);
        assert!(policy.allowed && !policy.default_on);

        let empty = write(dir.path(), "empty.yaml", "");
        assert!(policy_from(None, None, &[empty]).allowed);
        let other = write(dir.path(), "other.yaml", "future_section:\n  x: 1\n");
        assert!(policy_from(None, None, &[other]).allowed);

        // Unreadable or mistyped policies fail closed.
        for (name, text) in [
            ("typo.yaml", "updates:\n  chek: false\n"),
            ("type.yaml", "updates:\n  check: maybe\n"),
            ("broken.yaml", "updates: [\n"),
        ] {
            let file = write(dir.path(), name, text);
            let policy = policy_from(None, None, std::slice::from_ref(&file));
            assert!(!policy.allowed, "{name}");
            assert!(policy.message.unwrap().contains("could not be read"));
        }
        let huge = write(dir.path(), "huge.yaml", &"#".repeat(70_000));
        assert!(!policy_from(None, None, &[huge]).allowed);
        assert!(!policy_from(None, None, &[dir.path().to_path_buf()]).allowed);

        let long = "x".repeat(1000);
        assert_eq!(
            clean_message(&long).unwrap().chars().count(),
            MAX_MESSAGE_CHARS
        );
        assert_eq!(clean_message(" \n\t "), None);
        assert_eq!(clean_message("a\u{7}b").as_deref(), Some("a b"));
    }

    #[test]
    fn user_setting_follows_the_packaged_default() {
        let mut config = Config::default();
        // The key is always present for `shadowcode config updates.check`.
        assert_eq!(
            serde_json::to_value(&config).unwrap()["updates"],
            json!({"check": null})
        );
        let open = policy_from(None, None, &[]);
        let default_off = policy_from(Some("default-off"), None, &[]);
        let closed = policy_from(Some("off"), None, &[]);
        assert!(automatic(&config, &open));
        assert!(!automatic(&config, &default_off));
        config.updates.check = Some(true);
        assert!(automatic(&config, &default_off));
        assert!(!automatic(&config, &closed));
        config.updates.check = Some(false);
        assert!(!automatic(&config, &open));
    }

    #[test]
    fn install_type_is_detected_from_the_environment() {
        let detect = |bundle, appimage: Option<&str>, appdir: Option<&str>, exe: Option<&str>| {
            detect_install(&InstallProbe {
                bundle,
                appimage: appimage.map(PathBuf::from),
                appdir: appdir.map(PathBuf::from),
                exe: exe.map(PathBuf::from),
            })
        };
        let image = Some("/home/u/Applications/ShadowCode.AppImage");
        let extracted = Some("/tmp/appimage_extracted_1");
        assert_eq!(
            detect(
                None,
                image,
                extracted,
                Some("/tmp/appimage_extracted_1/usr/bin/shadowcode")
            ),
            InstallKind::Appimage
        );
        // APPIMAGE left in the environment of a different executable.
        assert_eq!(
            detect(
                Some(InstallKind::Deb),
                image,
                extracted,
                Some("/usr/bin/shadowcode")
            ),
            InstallKind::Deb
        );
        assert_eq!(
            detect(None, image, extracted, Some("/usr/bin/shadowcode")),
            InstallKind::System
        );
        // The bundler's marker in the executable.
        assert_eq!(
            detect(
                Some(InstallKind::Deb),
                None,
                None,
                Some("/usr/bin/shadowcode")
            ),
            InstallKind::Deb
        );
        assert_eq!(
            detect(
                Some(InstallKind::Appimage),
                None,
                None,
                Some("/tmp/x/usr/bin/shadowcode")
            ),
            InstallKind::Appimage
        );
        // No marker: where the executable lives.
        for (exe, kind) in [
            ("/usr/bin/shadowcode", InstallKind::System),
            ("/opt/shadowcode/bin/shadowcode", InstallKind::System),
            ("/usr/local/bin/shadowcode", InstallKind::Unknown),
            (
                "/home/u/src/ShadowCode/target/debug/shadowcode",
                InstallKind::Source,
            ),
            (
                "/home/u/src/ShadowCode/target/release/shadowcode",
                InstallKind::Source,
            ),
            ("/home/u/bin/shadowcode", InstallKind::Unknown),
        ] {
            assert_eq!(detect(None, None, None, Some(exe)), kind, "{exe}");
        }
        assert_eq!(detect(None, None, None, None), InstallKind::Unknown);
        assert_eq!(InstallKind::System.label(), "System package");
    }

    #[test]
    fn update_steps_match_the_install_type() {
        let open = policy_from(None, None, &[]);
        let release = parse_release(&fixture("latest-signed.json")).unwrap();
        let appimage = next_step(InstallKind::Appimage, &open, Some(&release));
        let text = appimage["text"].as_str().unwrap();
        assert!(text.contains("ShadowCode_0.34.0_amd64.AppImage"));
        assert!(text.contains("RELEASE-AUTH.sig"));
        assert_eq!(
            appimage["command"],
            "bash /path/to/trusted-bundle/scripts/install-appimage.sh ~/Downloads/ShadowCode_0.34.0_amd64.AppImage"
        );
        assert_eq!(
            appimage["link"],
            "https://github.com/Shadowfetchapps/ShadowCode/blob/v0.34.0/README.md#appimage-recommended"
        );
        let unsigned = Release {
            signed: false,
            ..release.clone()
        };
        let refused = next_step(InstallKind::Appimage, &open, Some(&unsigned));
        assert!(refused["text"].as_str().unwrap().contains("will refuse it"));
        assert!(refused["command"].is_null());

        let deb = next_step(InstallKind::Deb, &open, Some(&release));
        assert!(deb["text"]
            .as_str()
            .unwrap()
            .starts_with("Update through your package manager"));
        assert!(deb["command"].is_null());
        let source = next_step(InstallKind::Source, &open, Some(&release));
        assert_eq!(
            source["command"],
            "git fetch --tags && git checkout v0.34.0"
        );
        for kind in [InstallKind::System, InstallKind::Unknown] {
            assert!(next_step(kind, &open, Some(&release))["command"].is_null());
        }

        let distro = policy_from(
            None,
            Some("ShadowCode updates arrive with Shadowfetch Linux updates."),
            &[],
        );
        let managed = next_step(InstallKind::Deb, &distro, Some(&release));
        assert_eq!(
            managed,
            json!({"text": "ShadowCode updates arrive with Shadowfetch Linux updates.", "command": null, "link": null})
        );
    }

    #[test]
    fn status_reports_newer_releases_and_hides_dismissed_ones() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::isolated(dir.path()).unwrap();
        let config = Config::default();
        let open = policy_from(None, None, &[]);
        let status0 = status(&paths, &config, &open, InstallKind::Deb);
        assert_eq!(status0["available"], false);
        assert!(status0["last_checked_at"].is_null());
        assert!(status0["next_step"].is_null());

        let newer = Release {
            version: "999.0.0".into(),
            tag: "v999.0.0".into(),
            url: format!("{RELEASES_URL}/tag/v999.0.0"),
            published_at: None,
            signed: true,
        };
        save_state(
            &paths,
            &State {
                last_attempt: 10.0,
                last_success: 10.0,
                latest: Some(newer.clone()),
                ..State::default()
            },
        )
        .unwrap();
        let shown = status(&paths, &config, &open, InstallKind::Deb);
        assert_eq!(shown["available"], true);
        assert_eq!(shown["dismissed"], false);
        assert_eq!(shown["latest"]["version"], "999.0.0");
        assert_eq!(shown["last_checked_at"], 10.0);
        assert!(shown["next_step"]["text"].is_string());

        dismiss(&paths, "999.0.0").unwrap();
        assert_eq!(
            status(&paths, &config, &open, InstallKind::Deb)["dismissed"],
            true
        );
        assert!(dismiss(&paths, "not a version").is_err());

        // A policy that turns checks off also hides what an earlier check found.
        let closed = policy_from(Some("off"), None, &[]);
        let hidden = status(&paths, &config, &closed, InstallKind::Deb);
        assert_eq!(hidden["available"], false);
        assert!(hidden["latest"].is_null());
        assert_eq!(hidden["allowed"], false);

        // After updating, the cached release is no longer newer.
        let mut state = load_state(&paths);
        state.latest = Some(Release {
            version: crate::VERSION.into(),
            ..newer
        });
        save_state(&paths, &state).unwrap();
        assert_eq!(
            status(&paths, &config, &open, InstallKind::Deb)["available"],
            false
        );

        // A damaged state file reads as "never checked".
        fs::write(state_path(&paths), b"{not json").unwrap();
        assert_eq!(load_state(&paths), State::default());
    }

    #[test]
    fn commits_are_validated() {
        assert!(valid_commit("e15c4480e65db5650af012bb2a9773dbe89acf84"));
        assert!(valid_commit("e15c448"));
        assert!(valid_commit("e15c448-dirty"));
        assert!(!valid_commit("E15C448"));
        assert!(!valid_commit("e15c4"));
        assert!(!valid_commit("main"));
        assert!(!valid_commit(""));
    }

    #[test]
    fn about_carries_the_notice() {
        assert!(NOTICE.contains("originally created by Shadowfetch"));
        assert!(NOTICE.contains("Apache License, Version 2.0"));
    }
}
