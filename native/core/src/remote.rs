//! Remote access: the application API, a live event stream and the web
//! interface over HTTP, so a phone or another computer can follow and steer
//! tasks, plus optional phone notifications through ntfy.
//!
//! One [`Manager`] per engine (shared by every forked `Service`) owns the
//! settings (`remote.json`), paired devices, one-time pairing codes, the
//! running server and the ntfy notifier. The engine owner (desktop window or
//! `shadowcode serve`) calls [`Manager::activate`] when it starts; the
//! server and notifier then live until the owner's control endpoint closes.
//!
//! Security model (see `SECURITY.md` and `docs/REMOTE.md`):
//! - Off by default; binds 127.0.0.1 unless the user picks another address.
//! - Every API and stream request needs a device access token in the
//!   `Authorization` header (never a cookie or URL), compared in constant
//!   time; repeated failures from one address are refused for a while.
//! - Browsers must be same-origin: there are no CORS headers and
//!   cross-origin requests are refused.
//! - Remote clients cannot manage remote access, reach terminals (unless the
//!   user allows it), read secret files or see recognizable credentials
//!   (`remote::policy`).
use crate::{config::Config, paths::AppPaths, service::Service};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

pub mod assets;
pub mod auth;
mod http;
pub mod ntfy;
pub mod policy;
pub mod settings;

pub use settings::Settings;

/// How often a device's "last used" time is written back at most.
const LAST_SEEN_INTERVAL: f64 = 60.0;

struct Running {
    address: SocketAddr,
    cancel: CancellationToken,
    /// Disconnects once the server has let go of its listening socket.
    closed: std::sync::mpsc::Receiver<()>,
}

#[derive(Default)]
struct Inner {
    settings: Option<Settings>,
    running: Option<Running>,
    /// The engine owner's lifetime; servers and the notifier stop with it.
    parent: Option<CancellationToken>,
    notifier: Option<CancellationToken>,
    last_error: Option<String>,
    ntfy_error: Option<String>,
    ntfy_sent: VecDeque<Instant>,
}

pub struct Manager {
    paths: AppPaths,
    inner: Mutex<Inner>,
    limiter: Mutex<auth::Limiter>,
    pairing: Mutex<auth::Pairing>,
}

/// A paired device, as the server knows it after authentication.
#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    pub name: String,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Manager {
    pub fn new(paths: AppPaths) -> Self {
        Self {
            paths,
            inner: Mutex::default(),
            limiter: Mutex::default(),
            pairing: Mutex::default(),
        }
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// The saved settings (read once, then cached).
    pub fn settings(&self) -> Result<Settings> {
        let mut inner = lock(&self.inner);
        if inner.settings.is_none() {
            inner.settings = Some(settings::load(&self.paths)?);
        }
        Ok(inner.settings.clone().unwrap_or_default())
    }

    fn update(&self, change: impl FnOnce(&mut Settings) -> Result<()>) -> Result<Settings> {
        let mut inner = lock(&self.inner);
        let mut next = match &inner.settings {
            Some(settings) => settings.clone(),
            None => settings::load(&self.paths)?,
        };
        change(&mut next)?;
        settings::save(&self.paths, &next)?;
        inner.settings = Some(next.clone());
        Ok(next)
    }

    /// Called once by the engine owner: start the phone notifier and, when
    /// remote access is on, the web server. Both stop when `parent` is
    /// cancelled. Problems are reported in [`Manager::status`], never fatal.
    pub fn activate(self: &Arc<Self>, base: &Service, parent: CancellationToken) {
        {
            let mut inner = lock(&self.inner);
            inner.parent = Some(parent.clone());
            if let Some(old) = inner.notifier.take() {
                old.cancel();
            }
            let cancel = parent.child_token();
            inner.notifier = Some(cancel.clone());
            // Subscribe now so no broadcast after activation is missed.
            let events = base.engine.subscribe();
            tokio::spawn(notifier(self.clone(), base.clone(), events, cancel));
        }
        match self.settings() {
            Ok(settings) if settings.enabled => {
                if let Err(error) = self.start(base, None) {
                    tracing::warn!("Remote access did not start: {error:#}");
                }
            }
            Ok(_) => {}
            Err(error) => lock(&self.inner).last_error = Some(format!("{error:#}")),
        }
    }

    /// Start (or restart) the server on the saved address, or on `bind` for
    /// this run only (`shadowcode serve --remote-address`).
    pub fn start(self: &Arc<Self>, base: &Service, bind: Option<SocketAddr>) -> Result<SocketAddr> {
        // A restart waits (briefly) until the old server has closed its
        // socket, or binding the same port again fails as "in use".
        let previous = lock(&self.inner).running.take();
        if let Some(previous) = previous {
            previous.cancel.cancel();
            let _ = previous
                .closed
                .recv_timeout(std::time::Duration::from_secs(3));
        }
        let result = (|| {
            let settings = self.settings()?;
            let address = match bind {
                Some(address) => address,
                None => SocketAddr::new(
                    settings
                        .address
                        .parse()
                        .context("The saved remote access address is not an IP address")?,
                    settings.port,
                ),
            };
            let listener = std::net::TcpListener::bind(address).with_context(|| {
                format!("Could not listen on {address}; choose another port or address")
            })?;
            listener.set_nonblocking(true)?;
            let listener = tokio::net::TcpListener::from_std(listener)?;
            let bound = listener.local_addr()?;
            let parent = lock(&self.inner).parent.clone();
            let cancel = parent.map(|p| p.child_token()).unwrap_or_default();
            let shared = http::Shared::new(base.clone(), self.clone(), cancel.clone());
            let (closing, closed) = std::sync::mpsc::channel();
            tokio::spawn(http::serve(listener, shared, closing));
            lock(&self.inner).running = Some(Running {
                address: bound,
                cancel,
                closed,
            });
            Ok(bound)
        })();
        let mut inner = lock(&self.inner);
        inner.last_error = result.as_ref().err().map(|e| format!("{e:#}"));
        result
    }

    /// Stop the server; paired devices stay paired.
    pub fn stop(&self) {
        if let Some(running) = lock(&self.inner).running.take() {
            running.cancel.cancel();
        }
    }

    /// The address the server listens on, while it runs.
    pub fn address(&self) -> Option<SocketAddr> {
        lock(&self.inner).running.as_ref().map(|r| r.address)
    }

    pub fn allow_terminals(&self) -> bool {
        self.settings().is_ok_and(|s| s.allow_terminals)
    }

    /// Change the switches. Turning it on (or changing the address) starts
    /// or restarts the server; turning it off stops it.
    pub fn configure(self: &Arc<Self>, base: &Service, patch: &Value) -> Result<Value> {
        let before = self.settings()?;
        let next = self.update(|s| {
            if let Some(enabled) = patch["enabled"].as_bool() {
                s.enabled = enabled;
                if enabled {
                    s.confirm_restored();
                }
            }
            if let Some(address) = patch["address"].as_str() {
                s.address = validate_address(address)?.to_string();
            }
            if let Some(port) = patch.get("port").filter(|v| !v.is_null()) {
                let port = port
                    .as_u64()
                    .filter(|p| (1024..=65535).contains(p))
                    .context("Choose a port from 1024 to 65535")?;
                s.port = port as u16;
            }
            if let Some(url) = patch["public_url"].as_str() {
                s.public_url = validate_public_url(url)?;
            }
            if let Some(allow) = patch["allow_terminals"].as_bool() {
                s.allow_terminals = allow;
            }
            Ok(())
        })?;
        let rebind = next.address != before.address || next.port != before.port;
        let running = self.address().is_some();
        if patch["enabled"] == false {
            self.stop();
        } else if (next.enabled && !running) || (running && rebind) {
            // The switch is saved even if the port is busy; the page shows why.
            let _ = self.start(base, None);
        }
        Ok(self.status())
    }

    /// Everything the Settings page shows. Never includes tokens.
    pub fn status(&self) -> Value {
        let settings = self.settings().unwrap_or_default();
        let (address, last_error, ntfy_error) = {
            let inner = lock(&self.inner);
            (
                inner.running.as_ref().map(|r| r.address),
                inner.last_error.clone(),
                inner.ntfy_error.clone(),
            )
        };
        let exposed = settings
            .address
            .parse::<IpAddr>()
            .is_ok_and(|ip| !ip.is_loopback());
        let token_saved = crate::config::secret(&self.paths, ntfy::TOKEN_SECRET)
            .ok()
            .flatten()
            .is_some();
        json!({
            "enabled": settings.enabled,
            "running": address.is_some(),
            "address": settings.address,
            "port": settings.port,
            "bound": address.map(|a| a.to_string()),
            "url": address.map(|a| link_base(&settings, a, None)),
            "public_url": settings.public_url,
            "exposed": exposed,
            "allow_terminals": settings.allow_terminals,
            "error": last_error,
            "addresses": local_addresses(),
            "devices": settings.devices.iter().map(|d| json!({
                "id": d.id, "name": d.name, "created_at": d.created_at, "last_seen": d.last_seen,
                "restored": d.restored,
            })).collect::<Vec<_>>(),
            "ntfy": {
                "server": settings.ntfy.server,
                "topic": settings.ntfy.topic,
                "details": settings.ntfy.details,
                "events": settings.ntfy.events,
                "token_saved": token_saved,
                "configured": settings.ntfy.configured(),
                "restored": settings.ntfy.restored,
                "error": ntfy_error,
            },
        })
    }

    /// A one-time pairing link (valid 10 minutes) and its QR code. `host`
    /// picks which of this computer's addresses the link uses when the
    /// server listens on all of them.
    pub fn pair(&self, host: Option<&str>) -> Result<Value> {
        let address = self
            .address()
            .context("Turn on remote access before pairing a device")?;
        let settings = self.settings()?;
        let host = match host.filter(|h| !h.is_empty()) {
            Some(host) => {
                let ip: IpAddr = host.parse().context("Choose one of the listed addresses")?;
                ensure!(
                    local_ips().contains(&ip),
                    "Choose one of this computer's addresses"
                );
                Some(ip)
            }
            None => None,
        };
        let code = lock(&self.pairing).issue()?;
        let base = link_base(&settings, address, host);
        let link = format!("{base}/#pair={code}");
        Ok(json!({
            "link": link,
            "base": base,
            "expires_in": auth::PAIRING_TTL.as_secs(),
            "qr": qr_matrix(&link)?,
        }))
    }

    /// Exchange a pairing code for a new device token. The token is
    /// returned once and only its digest is kept.
    pub fn redeem(&self, code: &str, name: &str) -> Result<(String, Device)> {
        ensure!(
            !code.is_empty() && code.len() <= 128 && lock(&self.pairing).redeem(code),
            "This pairing link is not valid any more. Create a new one on the computer running ShadowCode."
        );
        let token = auth::new_token()?;
        let name: String = name
            .chars()
            .filter(|c| !c.is_control())
            .take(60)
            .collect::<String>()
            .trim()
            .to_owned();
        let device = settings::Device {
            id: crate::id(),
            name: if name.is_empty() {
                "Browser".into()
            } else {
                name
            },
            digest: auth::hex(&auth::digest(&token)),
            created_at: crate::now(),
            last_seen: Some(crate::now()),
            restored: false,
        };
        let paired = Device {
            id: device.id.clone(),
            name: device.name.clone(),
        };
        self.update(|s| {
            s.devices.push(device);
            while s.devices.len() > settings::MAX_DEVICES {
                // The least recently used device makes room.
                let oldest = s
                    .devices
                    .iter()
                    .enumerate()
                    .min_by(|a, b| {
                        let seen = |d: &settings::Device| d.last_seen.unwrap_or(d.created_at);
                        seen(a.1).total_cmp(&seen(b.1))
                    })
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                s.devices.remove(oldest);
            }
            Ok(())
        })?;
        Ok((token, paired))
    }

    /// The device a token belongs to. Every stored digest is compared. A
    /// device that came back with a restore is refused until the user turns
    /// remote access on (`shadowcode serve --remote` alone does not).
    pub fn authenticate(&self, token: &str) -> Option<Device> {
        let settings = self.settings().ok()?;
        let digests: Vec<[u8; 32]> = settings
            .devices
            .iter()
            .map(|d| auth::unhex(&d.digest).unwrap_or([0; 32]))
            .collect();
        let index = auth::find_digest(token, &digests)?;
        let device = &settings.devices[index];
        if device.restored {
            return None;
        }
        let now = crate::now();
        if device
            .last_seen
            .is_none_or(|seen| now - seen > LAST_SEEN_INTERVAL)
        {
            let id = device.id.clone();
            let _ = self.update(|s| {
                if let Some(d) = s.devices.iter_mut().find(|d| d.id == id) {
                    d.last_seen = Some(now);
                }
                Ok(())
            });
        }
        Some(Device {
            id: device.id.clone(),
            name: device.name.clone(),
        })
    }

    /// Still paired? (Streams re-check this so a revoked device drops off.)
    pub fn paired(&self, id: &str) -> bool {
        self.settings()
            .is_ok_and(|s| s.devices.iter().any(|d| d.id == id && !d.restored))
    }

    /// Unpair one device, or every device and every unused pairing link.
    pub fn revoke(&self, id: Option<&str>) -> Result<Value> {
        self.update(|s| {
            match id {
                Some(id) => {
                    let before = s.devices.len();
                    s.devices.retain(|d| d.id != id);
                    ensure!(s.devices.len() < before, "That device is not paired");
                }
                None => s.devices.clear(),
            }
            Ok(())
        })?;
        if id.is_none() {
            lock(&self.pairing).clear();
        }
        Ok(self.status())
    }

    pub(crate) fn limiter(&self) -> std::sync::MutexGuard<'_, auth::Limiter> {
        lock(&self.limiter)
    }

    /// Save phone notification settings. `token` (when present) goes to the
    /// secret store; an empty token removes it.
    pub fn set_ntfy(&self, patch: &Value) -> Result<Value> {
        self.update(|s| {
            let ntfy = &mut s.ntfy;
            if let Some(server) = patch["server"].as_str() {
                ntfy.server = if server.trim().is_empty() {
                    String::new()
                } else {
                    ntfy::validate_server(server)?
                };
            }
            if let Some(topic) = patch["topic"].as_str() {
                ntfy.topic = if topic.trim().is_empty() {
                    String::new()
                } else {
                    ntfy::validate_topic(topic)?
                };
            }
            // Saving the server and topic confirms settings that came back
            // with a restore.
            if patch["server"].is_string() || patch["topic"].is_string() {
                ntfy.restored = false;
            }
            if let Some(details) = patch["details"].as_bool() {
                ntfy.details = details;
            }
            let events = &patch["events"];
            for (key, slot) in [
                ("approval", &mut ntfy.events.approval),
                ("finished", &mut ntfy.events.finished),
                ("failed", &mut ntfy.events.failed),
                ("limit", &mut ntfy.events.limit),
            ] {
                if let Some(on) = events[key].as_bool() {
                    *slot = on;
                }
            }
            Ok(())
        })?;
        if let Some(token) = patch["token"].as_str() {
            crate::config::set_secret(&self.paths, ntfy::TOKEN_SECRET, token.trim())?;
        }
        lock(&self.inner).ntfy_error = None;
        Ok(self.status())
    }

    /// Send a test message now (ignores the per-kind switches).
    pub async fn test_ntfy(&self) -> Result<Value> {
        let settings = self.settings()?;
        ensure!(
            settings.ntfy.configured(),
            "Enter the ntfy server address and a topic first"
        );
        ensure!(
            !Config::load(&self.paths, None)?.offline(),
            "Offline mode is on; phone notifications are not sent"
        );
        let message = json!({
            "topic": settings.ntfy.topic,
            "title": "ShadowCode",
            "message": "Phone notifications are working.",
            "tags": ["white_check_mark"],
        });
        let token = crate::config::secret(&self.paths, ntfy::TOKEN_SECRET)?;
        let result = ntfy::publish(
            &reqwest::Client::new(),
            &settings.ntfy.server,
            token.as_deref(),
            &message,
        )
        .await;
        lock(&self.inner).ntfy_error = result.as_ref().err().map(|e| format!("{e:#}"));
        result?;
        Ok(json!({"ok": true}))
    }

    /// Where links in notifications point, when the web interface is
    /// reachable: the public address, else the running server.
    fn notification_base(&self, settings: &Settings) -> Option<String> {
        if !settings.public_url.is_empty() {
            return Some(settings.public_url.clone());
        }
        self.address().map(|a| link_base(settings, a, None))
    }

    /// Room for one more message in the burst window?
    fn ntfy_budget(&self) -> bool {
        let mut inner = lock(&self.inner);
        let now = Instant::now();
        while inner
            .ntfy_sent
            .front()
            .is_some_and(|t| now.duration_since(*t) > ntfy::BURST_WINDOW)
        {
            inner.ntfy_sent.pop_front();
        }
        if inner.ntfy_sent.len() >= ntfy::BURST_LIMIT {
            return false;
        }
        inner.ntfy_sent.push_back(now);
        true
    }
}

/// Follow engine broadcasts and send phone notifications for the ones the
/// shared decision (`crate::notify::select`) and the phone switches allow.
async fn notifier(
    manager: Arc<Manager>,
    service: Service,
    mut events: tokio::sync::broadcast::Receiver<Value>,
    cancel: CancellationToken,
) {
    let client = reqwest::Client::new();
    loop {
        let event = tokio::select! {
            _ = cancel.cancelled() => return,
            received = events.recv() => match received {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },
        };
        // Cheap check first: most broadcasts never notify.
        if crate::notify::select(&event, &crate::notify::Prefs::default()).is_none() {
            continue;
        }
        let Ok(settings) = manager.settings() else {
            continue;
        };
        // Settings that came back with a restore wait until the user checks
        // them; remote access being off does not stop notifications.
        if !settings.ntfy.configured() || settings.ntfy.restored {
            continue;
        }
        let Some(notice) = crate::notify::select(&event, &ntfy::prefs(&settings.ntfy)) else {
            continue;
        };
        if Config::load(&manager.paths, None).is_ok_and(|c| c.offline()) || !manager.ntfy_budget() {
            continue;
        }
        let project = (!notice.session_id.is_empty())
            .then(|| {
                service
                    .engine
                    .store()
                    .session(&notice.session_id)
                    .ok()
                    .flatten()
            })
            .flatten()
            .and_then(|s| {
                s["workspace"]
                    .as_str()
                    .and_then(|w| std::path::Path::new(w).file_name())
                    .map(|n| n.to_string_lossy().into_owned())
            });
        let base = manager.notification_base(&settings);
        let message = ntfy::message(&notice, &settings.ntfy, project.as_deref(), base.as_deref());
        let token = crate::config::secret(&manager.paths, ntfy::TOKEN_SECRET)
            .ok()
            .flatten();
        let (manager, client) = (manager.clone(), client.clone());
        tokio::spawn(async move {
            let result =
                ntfy::publish(&client, &settings.ntfy.server, token.as_deref(), &message).await;
            lock(&manager.inner).ntfy_error = result.err().map(|e| format!("{e:#}"));
        });
    }
}

/// An address the user may bind: any of this computer's addresses, or all
/// of them (`0.0.0.0` / `::`).
pub fn validate_address(text: &str) -> Result<IpAddr> {
    let ip: IpAddr = text
        .trim()
        .parse()
        .context("Enter an IP address such as 127.0.0.1")?;
    ensure!(
        ip.is_unspecified() || ip.is_loopback() || local_ips().contains(&ip),
        "{ip} is not an address of this computer"
    );
    Ok(ip)
}

fn validate_public_url(text: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(String::new());
    }
    let url = reqwest::Url::parse(text)
        .context("Enter a full address such as https://box.tailnet.ts.net")?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "The public address must start with https:// or http://"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "The public address cannot include a user name, query or fragment"
    );
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// `http://host:port` for links, preferring the public address.
fn link_base(settings: &Settings, bound: SocketAddr, host: Option<IpAddr>) -> String {
    if !settings.public_url.is_empty() && host.is_none() {
        return settings.public_url.clone();
    }
    let ip = host.unwrap_or_else(|| {
        if bound.ip().is_unspecified() {
            preferred_ip().unwrap_or(IpAddr::from([127, 0, 0, 1]))
        } else {
            bound.ip()
        }
    });
    match ip {
        IpAddr::V6(v6) => format!("http://[{v6}]:{}", bound.port()),
        IpAddr::V4(v4) => format!("http://{v4}:{}", bound.port()),
    }
}

/// A Tailscale address first, else a LAN address.
fn preferred_ip() -> Option<IpAddr> {
    let addresses = interfaces();
    addresses
        .iter()
        .find(|(_, ip)| is_tailscale(ip))
        .or_else(|| addresses.iter().find(|(_, ip)| !ip.is_loopback()))
        .map(|(_, ip)| *ip)
}

fn is_tailscale(ip: &IpAddr) -> bool {
    match ip {
        // 100.64.0.0/10 (CGNAT), which Tailscale uses for its addresses.
        IpAddr::V4(v4) => v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64,
        IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

fn local_ips() -> Vec<IpAddr> {
    interfaces().into_iter().map(|(_, ip)| ip).collect()
}

/// This computer's addresses with a kind the Settings page can explain.
pub fn local_addresses() -> Vec<Value> {
    interfaces()
        .into_iter()
        .map(|(name, ip)| {
            let kind = if ip.is_loopback() {
                "loopback"
            } else if is_tailscale(&ip) || name.starts_with("tailscale") {
                "tailscale"
            } else {
                "lan"
            };
            json!({"address": ip.to_string(), "interface": name, "kind": kind})
        })
        .collect()
}

/// `(interface, address)` pairs from getifaddrs, without IPv6 link-local
/// addresses (they need a zone and do not work in links).
#[cfg(unix)]
fn interfaces() -> Vec<(String, IpAddr)> {
    let mut found = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list that is released
    // with freeifaddrs below; it is only read in between.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return found;
    }
    let mut cursor = list;
    while !cursor.is_null() {
        // SAFETY: `cursor` is a node of the list returned by getifaddrs.
        let entry = unsafe { &*cursor };
        cursor = entry.ifa_next;
        if entry.ifa_addr.is_null() || entry.ifa_name.is_null() {
            continue;
        }
        // SAFETY: ifa_name is a NUL-terminated string owned by the list.
        let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: ifa_addr is non-null; its family says which sockaddr it is.
        let family = unsafe { (*entry.ifa_addr).sa_family } as i32;
        let ip = match family {
            libc::AF_INET => {
                // SAFETY: AF_INET addresses are sockaddr_in.
                let addr = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in) };
                IpAddr::from(u32::from_be(addr.sin_addr.s_addr).to_be_bytes())
            }
            libc::AF_INET6 => {
                // SAFETY: AF_INET6 addresses are sockaddr_in6.
                let addr = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in6) };
                let ip = std::net::Ipv6Addr::from(addr.sin6_addr.s6_addr);
                if ip.segments()[0] & 0xffc0 == 0xfe80 {
                    continue;
                }
                IpAddr::V6(ip)
            }
            _ => continue,
        };
        if !found.iter().any(|(_, known)| *known == ip) {
            found.push((name, ip));
        }
    }
    // SAFETY: `list` came from getifaddrs and is freed exactly once.
    unsafe { libc::freeifaddrs(list) };
    found
}

#[cfg(not(unix))]
fn interfaces() -> Vec<(String, IpAddr)> {
    vec![("lo".into(), IpAddr::from([127, 0, 0, 1]))]
}

/// The QR code of `text` as rows of `0`/`1` (dark) modules.
pub fn qr_matrix(text: &str) -> Result<Value> {
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::M)
        .context("The link is too long for a QR code")?;
    let width = code.width();
    let colors = code.to_colors();
    let rows: Vec<String> = colors
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(|c| if *c == qrcode::Color::Dark { '1' } else { '0' })
                .collect()
        })
        .collect();
    Ok(json!({"size": width, "rows": rows}))
}

/// The QR code of `text` drawn with block characters for a terminal
/// (light modules drawn dark so it scans on dark and light themes alike
/// with the usual quiet zone).
pub fn qr_terminal(text: &str) -> Result<String> {
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::L)
        .context("The link is too long for a QR code")?;
    let width = code.width();
    let colors = code.to_colors();
    let dark = |x: isize, y: isize| -> bool {
        x >= 0
            && y >= 0
            && (x as usize) < width
            && (y as usize) < width
            && colors[y as usize * width + x as usize] == qrcode::Color::Dark
    };
    let quiet = 2isize;
    let size = width as isize;
    let mut out = String::new();
    let mut y = -quiet;
    while y < size + quiet {
        for x in -quiet..size + quiet {
            // Upper half block: foreground = top module, background = bottom.
            out.push(match (dark(x, y), dark(x, y + 1)) {
                (false, false) => '█',
                (false, true) => '▀',
                (true, false) => '▄',
                (true, true) => ' ',
            });
        }
        out.push('\n');
        y += 2;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_and_links() {
        assert!(validate_address("127.0.0.1").is_ok());
        assert!(validate_address("0.0.0.0").is_ok());
        assert!(validate_address("::1").is_ok());
        assert!(validate_address("203.0.113.9").is_err());
        assert!(validate_address("localhost").is_err());
        assert!(is_tailscale(&"100.101.102.103".parse().unwrap()));
        assert!(!is_tailscale(&"100.128.0.1".parse().unwrap()));
        assert!(is_tailscale(&"fd7a:115c:a1e0::1".parse().unwrap()));
        let settings = Settings::default();
        let bound: SocketAddr = "127.0.0.1:7390".parse().unwrap();
        assert_eq!(link_base(&settings, bound, None), "http://127.0.0.1:7390");
        let v6: SocketAddr = "[::1]:7390".parse().unwrap();
        assert_eq!(link_base(&settings, v6, None), "http://[::1]:7390");
        let public = Settings {
            public_url: "https://box.tailnet.ts.net".into(),
            ..Settings::default()
        };
        assert_eq!(
            link_base(&public, bound, None),
            "https://box.tailnet.ts.net"
        );
        assert_eq!(
            validate_public_url("https://box.tailnet.ts.net/").unwrap(),
            "https://box.tailnet.ts.net"
        );
        assert!(validate_public_url("https://u:p@box").is_err());
        assert!(validate_public_url("javascript:alert(1)").is_err());
        assert!(local_addresses()
            .iter()
            .any(|a| a["kind"] == "loopback" || a["address"] == "127.0.0.1"));
    }

    #[test]
    fn qr_codes_render() {
        let matrix = qr_matrix("http://127.0.0.1:7390/#pair=abc").unwrap();
        let size = matrix["size"].as_u64().unwrap() as usize;
        let rows = matrix["rows"].as_array().unwrap();
        assert!(size >= 21 && rows.len() == size);
        assert!(rows.iter().all(|r| r.as_str().unwrap().len() == size));
        // Finder pattern: the top-left corner module is dark.
        assert!(rows[0].as_str().unwrap().starts_with("1111111"));
        let text = qr_terminal("http://127.0.0.1:7390/#pair=abc").unwrap();
        assert!(text.lines().count() >= size / 2);
    }
}
