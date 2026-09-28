//! Google's official Antigravity ACP server (`agy_acp_server.par`), the
//! runtime ShadowCode uses for Antigravity. Unlike `agy --print`, it speaks
//! the Agent Client Protocol and sends `session/request_permission`, so
//! Antigravity's actions go through ShadowCode's approvals.
//!
//! - Distribution: the ACP registry entry
//!   (https://github.com/agentclientprotocol/registry, `antigravity-acp`)
//!   points at a zip on dl.google.com. ShadowCode downloads it only when the
//!   user chooses Install, checks the pinned size and SHA-256, and unpacks it
//!   under `$XDG_DATA_HOME/shadowcode/antigravity-acp/<version>`.
//! - Profile: the server gets a private `GEMINI_HOME` owned by ShadowCode, so
//!   its sign-in (Google OAuth, stored in a file there) is separate from the
//!   `agy` CLI and the Antigravity app. Disconnect deletes that profile.
//! - Browser: task and status runs set `BROWSER` to a no-op, so the server
//!   can never open a sign-in page behind the user's back; it prints the link
//!   instead, which ShadowCode treats as "sign in required". Only Connect
//!   lets it open the browser.
//! - Temp files: the server writes logs and certificate bundles to `TMPDIR`;
//!   each launch gets its own directory, removed afterwards.
use anyhow::{bail, ensure, Context, Result};
use flate2::read::DeflateDecoder;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

pub const VERSION: &str = "1.2.1";
pub const DOWNLOAD_URL: &str =
    "https://dl.google.com/agy-extensions/releases/linux/agy-acp-server-1.2.1-linux-x86_64.zip";
pub const ARCHIVE_SHA256: &str = "9fbf0bd584a26478161f637cabd75113f72541c842d148f578ef1a6a9edcb843";
pub const ARCHIVE_BYTES: u64 = 333_590_110;
/// Unpacked size, shown before installing.
pub const INSTALLED_BYTES: u64 = 919_951_920 + 132_815_192;
pub const SERVER_FILE: &str = "agy_acp_server.par";
pub const HARNESS_FILE: &str = "localharness_external";
/// The OAuth method for a personal Google account.
pub const AUTH_METHOD: &str = "oauth-personal";
/// Printed on stderr when the server wants an interactive Google sign-in.
const SIGN_IN_MARKERS: [&str; 2] = [
    "to authenticate the ACP server",
    "accounts.google.com/o/oauth2",
];
/// Credentials for other Google auth methods never reach the server.
const REMOVED_ENV: [&str; 17] = [
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_CLOUD_LOCATION",
    "GOOGLE_CLOUD_QUOTA_PROJECT",
    "GOOGLE_GENAI_USE_VERTEXAI",
    "GCLOUD_PROJECT",
    "CLOUDSDK_CORE_PROJECT",
    "AGY_ACP_CCPA_PROJECT",
    "AGY_ACP_ENABLE_OAUTH",
    "GEMINI_HOME",
    "AGY_ACP_FORCE_FILE_STORAGE",
    "ANTIGRAVITY_HARNESS_PATH",
    "BROWSER",
    "PYTHONUNBUFFERED",
    "TMPDIR",
];

/// `$SHADOWCODE_ANTIGRAVITY_HOME` (tests) or
/// `$XDG_DATA_HOME/shadowcode/antigravity-acp`.
pub fn home() -> PathBuf {
    if let Some(dir) = std::env::var_os("SHADOWCODE_ANTIGRAVITY_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    data.join("shadowcode").join("antigravity-acp")
}

pub fn install_dir() -> PathBuf {
    home().join(VERSION)
}

/// The private `GEMINI_HOME` holding the server's own sign-in.
pub fn profile_dir() -> PathBuf {
    home().join("profile")
}

fn runs_dir() -> PathBuf {
    home().join("runs")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Installation {
    pub server: PathBuf,
    pub harness: PathBuf,
}

/// The default `cli_agents.antigravity_binary` value (kept from the `agy`
/// CLI era): it means "use the managed installation".
pub const DEFAULT_SETTING: &str = "agy";

/// A server path set in `cli_agents.antigravity_binary` (with
/// `localharness_external` beside it), or the managed installation when the
/// setting is the default.
pub fn installation(configured: &str) -> Option<Installation> {
    let setting = configured.trim();
    let configured = Path::new(setting);
    if configured.file_name().is_some_and(|n| n == SERVER_FILE) && configured.is_file() {
        let harness = configured.with_file_name(HARNESS_FILE);
        if harness.is_file() {
            return Some(Installation {
                server: configured.to_owned(),
                harness,
            });
        }
    }
    if !(setting.is_empty() || setting == DEFAULT_SETTING) {
        return None;
    }
    let dir = install_dir();
    let found = Installation {
        server: dir.join(SERVER_FILE),
        harness: dir.join(HARNESS_FILE),
    };
    (found.server.is_file() && found.harness.is_file()).then_some(found)
}

/// This machine has an IPv6 loopback (`::1`). The server refuses to start
/// without one unless told to skip the check.
pub fn has_ipv6_loopback() -> bool {
    fs::read_to_string("/proc/net/if_inet6")
        .map(|text| {
            text.lines()
                .any(|l| l.starts_with("00000000000000000000000000000001"))
        })
        .unwrap_or(false)
}

pub fn launch_args() -> Vec<String> {
    let mut args = vec!["--uid=".to_owned()];
    if !has_ipv6_loopback() {
        // The server's own documented switch for hosts without `::1`.
        args.push("--enforce_kernel_ipv6_support=false".into());
    }
    args
}

pub fn is_sign_in_prompt(line: &str) -> bool {
    SIGN_IN_MARKERS.iter().any(|m| line.contains(m))
}

/// Profile directory with `settings.json` naming the personal sign-in
/// method (never a credential).
fn ensure_profile() -> Result<PathBuf> {
    let dir = profile_dir();
    let acp = dir.join("antigravity-acp");
    fs::create_dir_all(&acp).context("Could not create the Antigravity profile")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    let settings = acp.join("settings.json");
    let wanted = json!({"auth": {"type": AUTH_METHOD}});
    let current: Option<Value> = fs::read(&settings)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    if current.as_ref().map(|v| &v["auth"]["type"]) != Some(&wanted["auth"]["type"]) {
        let mut value = current.unwrap_or_else(|| json!({}));
        if !value.is_object() {
            value = json!({});
        }
        value["auth"] = wanted["auth"].clone();
        crate::paths::atomic_write(
            &settings,
            serde_json::to_string_pretty(&value)?.as_bytes(),
            true,
        )?;
    }
    Ok(dir)
}

/// A per-launch temp directory, removed when dropped.
pub struct RunDir(PathBuf);
impl RunDir {
    pub fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for RunDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Environment for one server launch. `allow_browser` is true only for
/// Connect, where the user asked to sign in.
pub fn prepare(
    command: &mut tokio::process::Command,
    installation: &Installation,
    allow_browser: bool,
) -> Result<RunDir> {
    let profile = ensure_profile()?;
    // Temp directories of launches a crash never cleaned up.
    sweep_runs();
    let runs = runs_dir();
    fs::create_dir_all(&runs)?;
    let run = RunDir(runs.join(crate::id()));
    fs::create_dir_all(run.path())?;
    for name in REMOVED_ENV {
        command.env_remove(name);
    }
    super::scrub_api_keys(command);
    command
        .env("GEMINI_HOME", &profile)
        .env("AGY_ACP_FORCE_FILE_STORAGE", "1")
        .env("ANTIGRAVITY_HARNESS_PATH", &installation.harness)
        .env("PYTHONUNBUFFERED", "1")
        .env("TMPDIR", run.path());
    if !allow_browser {
        // Python's webbrowser runs $BROWSER first; `true` opens nothing, so
        // the server only prints the sign-in link.
        command.env("BROWSER", "true");
    }
    Ok(run)
}

/// Remove run directories left by a crash (older than a day).
pub fn sweep_runs() {
    let Ok(entries) = fs::read_dir(runs_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() > 86_400);
        if old {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Disconnect: delete ShadowCode's private profile (the server's sign-in).
pub fn sign_out() -> Result<bool> {
    let dir = profile_dir();
    if !dir.exists() {
        return Ok(false);
    }
    fs::remove_dir_all(&dir).context("Could not remove the Antigravity profile")?;
    Ok(true)
}

// ---------------------------------------------------------------- install

#[derive(Clone, Debug, Default)]
struct Progress {
    state: String,
    done: u64,
    total: u64,
    error: Option<String>,
}

static PROGRESS: Mutex<Option<Progress>> = Mutex::new(None);

fn set_progress(state: &str, done: u64, total: u64, error: Option<String>) {
    if let Ok(mut slot) = PROGRESS.lock() {
        *slot = Some(Progress {
            state: state.into(),
            done,
            total,
            error,
        });
    }
}

/// Remove the managed installation and forget the last install result.
pub fn uninstall() -> Result<bool> {
    let dir = install_dir();
    let existed = dir.exists();
    if existed {
        fs::remove_dir_all(&dir).context("Could not remove the Antigravity agent")?;
    }
    if let Ok(mut slot) = PROGRESS.lock() {
        *slot = None;
    }
    Ok(existed)
}

/// Install state for the Accounts card.
pub fn install_status(configured: &str) -> Value {
    let installed = installation(configured);
    let progress = PROGRESS.lock().ok().and_then(|p| p.clone());
    let busy = progress
        .as_ref()
        .is_some_and(|p| matches!(p.state.as_str(), "downloading" | "verifying" | "unpacking"));
    json!({
        "installed": installed.is_some(),
        "version": VERSION,
        "path": installed.as_ref().map(|i| i.server.display().to_string()),
        "managed": installed.as_ref().is_some_and(|i| i.server.starts_with(install_dir())),
        "download_bytes": ARCHIVE_BYTES,
        "installed_bytes": INSTALLED_BYTES,
        "source": DOWNLOAD_URL,
        "dir": install_dir().display().to_string(),
        "state": progress.as_ref().map(|p| p.state.clone()).unwrap_or_else(|| if installed.is_some() {"installed".into()} else {"not_installed".into()}),
        "busy": busy,
        "done": progress.as_ref().map_or(0, |p| p.done),
        "total": progress.as_ref().map_or(0, |p| p.total),
        "error": progress.and_then(|p| p.error),
    })
}

/// Start the download in the background. Returns false if one is running.
pub fn start_install() -> bool {
    {
        let Ok(mut slot) = PROGRESS.lock() else {
            return false;
        };
        if slot
            .as_ref()
            .is_some_and(|p| matches!(p.state.as_str(), "downloading" | "verifying" | "unpacking"))
        {
            return false;
        }
        *slot = Some(Progress {
            state: "downloading".into(),
            done: 0,
            total: ARCHIVE_BYTES,
            error: None,
        });
    }
    tokio::spawn(async {
        match install(DOWNLOAD_URL, ARCHIVE_SHA256, ARCHIVE_BYTES, &install_dir()).await {
            Ok(()) => set_progress("installed", ARCHIVE_BYTES, ARCHIVE_BYTES, None),
            Err(error) => set_progress("error", 0, ARCHIVE_BYTES, Some(format!("{error:#}"))),
        }
    });
    true
}

/// Download, verify and unpack into `target` (replaced atomically).
pub async fn install(url: &str, sha256: &str, bytes: u64, target: &Path) -> Result<()> {
    let parent = target.parent().context("Install directory has no parent")?;
    fs::create_dir_all(parent)?;
    let partial = parent.join(format!(".download-{}", crate::id()));
    let result = download_and_unpack(url, sha256, bytes, &partial, target).await;
    let _ = fs::remove_file(&partial);
    result
}

async fn download_and_unpack(
    url: &str,
    sha256: &str,
    bytes: u64,
    partial: &Path,
    target: &Path,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .user_agent(concat!("ShadowCode/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let response = client
        .get(url)
        .send()
        .await
        .context("Could not reach dl.google.com")?;
    ensure!(
        response.status().is_success(),
        "The download returned HTTP {}",
        response.status().as_u16()
    );
    let mut file = fs::File::create(partial)?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let mut stream = futures_util::StreamExt::fuse(response.bytes_stream());
    while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
        let chunk = chunk.context("The download was interrupted")?;
        done += chunk.len() as u64;
        ensure!(done <= bytes, "The download is larger than expected");
        hasher.update(&chunk);
        file.write_all(&chunk)?;
        set_progress("downloading", done, bytes, None);
    }
    file.flush()?;
    drop(file);
    set_progress("verifying", done, bytes, None);
    ensure!(
        done == bytes,
        "The download is {done} bytes; expected {bytes}"
    );
    let digest = format!("{:x}", hasher.finalize());
    ensure!(
        digest == sha256,
        "The download's SHA-256 is {digest}; expected {sha256}"
    );
    set_progress("unpacking", done, bytes, None);
    let staging = target.with_file_name(format!(".unpack-{}", crate::id()));
    let archive = partial.to_owned();
    let stage = staging.clone();
    let unpacked = tokio::task::spawn_blocking(move || unzip(&archive, &stage)).await?;
    if let Err(error) = unpacked {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    for name in [SERVER_FILE, HARNESS_FILE] {
        ensure!(staging.join(name).is_file(), "The archive has no {name}");
    }
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    fs::rename(&staging, target)?;
    Ok(())
}

/// Minimal zip reader for the registry archive: stored or deflated entries,
/// no encryption, no zip64. Paths must be plain file names.
pub fn unzip(archive: &Path, into: &Path) -> Result<Vec<String>> {
    let mut file = fs::File::open(archive)?;
    let len = file.metadata()?.len();
    let tail = len.min(65_557);
    file.seek(SeekFrom::Start(len - tail))?;
    let mut end = vec![0u8; tail as usize];
    file.read_exact(&mut end)?;
    let eocd = end
        .windows(4)
        .rposition(|w| w == [0x50, 0x4b, 0x05, 0x06])
        .context("Not a zip archive")?;
    let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as u64;
    let u32_at =
        |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as u64;
    let entries = u16_at(&end, eocd + 10);
    let cd_size = u32_at(&end, eocd + 12);
    let cd_offset = u32_at(&end, eocd + 16);
    ensure!(
        cd_offset != 0xffff_ffff && entries != 0xffff,
        "zip64 archives are not supported"
    );
    ensure!(
        cd_size <= 1_000_000 && entries <= 64,
        "Unexpected zip directory"
    );
    file.seek(SeekFrom::Start(cd_offset))?;
    let mut cd = vec![0u8; cd_size as usize];
    file.read_exact(&mut cd)?;
    fs::create_dir_all(into)?;
    let mut names = Vec::new();
    let mut at = 0usize;
    for _ in 0..entries {
        ensure!(
            cd.len() >= at + 46 && cd[at..at + 4] == [0x50, 0x4b, 0x01, 0x02],
            "Bad zip directory entry"
        );
        let flags = u16_at(&cd, at + 8);
        let method = u16_at(&cd, at + 10);
        let crc = u32_at(&cd, at + 16) as u32;
        let packed = u32_at(&cd, at + 20);
        let size = u32_at(&cd, at + 24);
        let name_len = u16_at(&cd, at + 28) as usize;
        let extra_len = u16_at(&cd, at + 30) as usize;
        let comment_len = u16_at(&cd, at + 32) as usize;
        let external = u32_at(&cd, at + 38);
        let local = u32_at(&cd, at + 42);
        let name = String::from_utf8(cd[at + 46..at + 46 + name_len].to_vec())?;
        at += 46 + name_len + extra_len + comment_len;
        ensure!(flags & 0x1 == 0, "Encrypted zip entries are not supported");
        ensure!(
            !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != "..",
            "Unexpected path in the archive: {name}"
        );
        ensure!(
            packed != 0xffff_ffff && size != 0xffff_ffff,
            "zip64 entries are not supported"
        );
        file.seek(SeekFrom::Start(local))?;
        let mut header = [0u8; 30];
        file.read_exact(&mut header)?;
        ensure!(header[..4] == [0x50, 0x4b, 0x03, 0x04], "Bad local header");
        let skip = u16_at(&header, 26) + u16_at(&header, 28);
        file.seek(SeekFrom::Current(skip as i64))?;
        let limited = (&mut file).take(packed);
        let out_path = into.join(&name);
        let mut out = fs::File::create(&out_path)?;
        let mut hasher = crc32::Hasher::default();
        let written = match method {
            0 => copy_hashing(limited, &mut out, &mut hasher)?,
            8 => copy_hashing(DeflateDecoder::new(limited), &mut out, &mut hasher)?,
            other => bail!("Unsupported zip compression method {other}"),
        };
        ensure!(
            written == size,
            "{name}: unpacked {written} bytes, expected {size}"
        );
        ensure!(hasher.finish() == crc, "{name}: CRC mismatch");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Owner read/write, plus execute when the archive marks it.
            let mode = ((external >> 16) as u32) & 0o777;
            let exec = if mode & 0o111 != 0 { 0o700 } else { 0o600 };
            fs::set_permissions(&out_path, fs::Permissions::from_mode(exec))?;
        }
        names.push(name);
    }
    Ok(names)
}

fn copy_hashing(mut from: impl Read, to: &mut fs::File, hasher: &mut crc32::Hasher) -> Result<u64> {
    let mut buffer = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = from.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        to.write_all(&buffer[..n])?;
        total += n as u64;
    }
    Ok(total)
}

/// IEEE CRC-32 (zip), table-driven.
mod crc32 {
    const fn table() -> [u32; 256] {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    }
    const TABLE: [u32; 256] = table();
    #[derive(Default)]
    pub struct Hasher(u32);
    impl Hasher {
        pub fn update(&mut self, bytes: &[u8]) {
            let mut c = !self.0;
            for b in bytes {
                c = TABLE[((c ^ *b as u32) & 0xff) as usize] ^ (c >> 8);
            }
            self.0 = !c;
        }
        pub fn finish(&self) -> u32 {
            self.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        let mut h = crc32::Hasher::default();
        h.update(b"123456789");
        assert_eq!(h.finish(), 0xcbf4_3926);
    }

    #[test]
    fn sign_in_prompts_are_recognised() {
        assert!(is_sign_in_prompt(
            "Open the following link to authenticate the ACP server: https://accounts.google.com/o/oauth2/v2/auth?x=1"
        ));
        assert!(!is_sign_in_prompt("I0924 started"));
    }

    #[test]
    fn launch_args_always_pass_the_linux_uid_flag() {
        let args = launch_args();
        assert_eq!(args[0], "--uid=");
        assert_eq!(
            args.len() == 2,
            !has_ipv6_loopback(),
            "the IPv6 check is skipped only without ::1"
        );
    }
}
