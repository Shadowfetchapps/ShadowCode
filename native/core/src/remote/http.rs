//! The remote access HTTP server.
//!
//! | Path | Auth | What |
//! |---|---|---|
//! | `GET /`, `/assets/…`, `/manifest.webmanifest`, icons | none | the built web interface |
//! | `POST /_remote/pair` | one-time code in the body | exchange a pairing code for a device token |
//! | `GET /_remote/session` | token | the paired device and what it may use |
//! | `GET /_remote/stream` | token | Server-Sent Events: the desktop's wake-ups |
//! | `/api/…` | token | the application API (`Service::dispatch`), filtered by `policy` |
//!
//! Tokens travel only in `Authorization: Bearer …`. There are no cookies and
//! no CORS headers: a browser request carrying an `Origin` must come from
//! this server's own origin, and preflights are refused.
use super::{assets, auth, policy, Device, Manager};
use crate::service::{Request as ApiRequest, Service};
use bytes::Bytes;
use http_body::{Body, Frame};
use http_body_util::{combinators::BoxBody, BodyExt, Full, Limited};
use hyper::{
    body::Incoming,
    header::{self, HeaderMap, HeaderValue},
    server::conn::http1,
    service::service_fn,
    Method, Request, Response, StatusCode,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    convert::Infallible,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, Semaphore},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

type Reply = Response<BoxBody<Bytes, Infallible>>;

/// Largest API request body (attachments are sent inline, as on the desktop).
pub const API_BODY_LIMIT: usize = 8 * 1024 * 1024;
const PAIR_BODY_LIMIT: usize = 4096;
const RESPONSE_LIMIT: usize = 68_000_000;
const MAX_CONNECTIONS: usize = 64;
const MAX_STREAMS: usize = 32;
const MAX_VIEWS: usize = 32;
const VIEW_IDLE: Duration = Duration::from_secs(60 * 60);
const API_TIMEOUT: Duration = Duration::from_secs(600);
const KEEPALIVE: Duration = Duration::from_secs(20);
const HTML_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; manifest-src 'self'; media-src 'self' blob:; worker-src 'none'; object-src 'none'; base-uri 'self'; form-action 'none'; frame-ancestors 'none'";
const DATA_CSP: &str = "default-src 'none'; frame-ancestors 'none'; sandbox";
pub const VIEW_HEADER: &str = "x-shadow-view";

struct View {
    service: Service,
    used: Instant,
}

pub(super) struct Shared {
    base: Service,
    manager: Arc<Manager>,
    views: Mutex<HashMap<(String, String), View>>,
    assets: Option<Arc<dyn assets::UiAssets>>,
    streams: Arc<Semaphore>,
    cancel: CancellationToken,
}

impl Shared {
    pub(super) fn new(
        base: Service,
        manager: Arc<Manager>,
        cancel: CancellationToken,
    ) -> Arc<Self> {
        Self::with_assets(base, manager, cancel, assets::bundled())
    }
    pub(super) fn with_assets(
        base: Service,
        manager: Arc<Manager>,
        cancel: CancellationToken,
        assets: Option<Arc<dyn assets::UiAssets>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            base,
            manager,
            views: Mutex::default(),
            assets,
            streams: Arc::new(Semaphore::new(MAX_STREAMS)),
            cancel,
        })
    }

    /// The device's navigation state for one browser tab. Remote clients
    /// never change the project the desktop window shows.
    fn view(&self, device: &str, view: &str) -> anyhow::Result<Service> {
        let mut views = self.views.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let key = (device.to_owned(), view.to_owned());
        if let Some(found) = views.get_mut(&key) {
            found.used = now;
            return Ok(found.service.clone());
        }
        let mut dropped = Vec::new();
        views.retain(|(owner, _), v| {
            let keep = now.duration_since(v.used) < VIEW_IDLE && self.manager.paired(owner);
            if !keep {
                dropped.push(v.service.clone());
            }
            keep
        });
        while views.len() >= MAX_VIEWS {
            let Some(oldest) = views
                .iter()
                .min_by_key(|(_, v)| v.used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(v) = views.remove(&oldest) {
                dropped.push(v.service);
            }
        }
        let service = self.base.fork_selection(self.base.workspace()?, None)?;
        views.insert(
            key,
            View {
                service: service.clone(),
                used: now,
            },
        );
        drop(views);
        close_views(dropped);
        Ok(service)
    }
}

/// A dropped view's terminals end with it.
fn close_views(views: Vec<Service>) {
    if views.is_empty() {
        return;
    }
    tokio::task::spawn_blocking(move || {
        for view in views {
            view.close_terminals(Duration::from_millis(500));
        }
    });
}

/// Accept connections until the manager stops this server.
/// `closing` is dropped as soon as the listening socket is closed.
pub(super) async fn serve(
    listener: TcpListener,
    shared: Arc<Shared>,
    closing: std::sync::mpsc::Sender<()>,
) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = shared.cancel.cancelled() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else { continue };
                let Ok(permit) = slots.clone().try_acquire_owned() else { drop(stream); continue };
                let shared = shared.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let cancel = shared.cancel.clone();
                    let service = service_fn(move |request| {
                        let shared = shared.clone();
                        async move { Ok::<_, Infallible>(handle(&shared, peer, request).await) }
                    });
                    let mut builder = http1::Builder::new();
                    builder
                        .timer(TokioTimer::new())
                        .keep_alive(true)
                        .header_read_timeout(Duration::from_secs(10))
                        .max_headers(64)
                        .max_buf_size(64 * 1024);
                    let connection = builder.serve_connection(TokioIo::new(stream), service);
                    tokio::pin!(connection);
                    tokio::select! {
                        _ = cancel.cancelled() => {}
                        _ = &mut connection => {}
                    }
                });
            }
        }
    }
    drop(listener);
    drop(closing);
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    let views: Vec<Service> = shared
        .views
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain()
        .map(|(_, v)| v.service)
        .collect();
    close_views(views);
}

fn secure(mut response: Reply, csp: &str) -> Reply {
    let headers = response.headers_mut();
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        ("cross-origin-opener-policy", "same-origin"),
        ("cross-origin-resource-policy", "same-origin"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    if let Ok(value) = HeaderValue::from_str(csp) {
        headers.insert(header::CONTENT_SECURITY_POLICY, value);
    }
    if !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

fn json_reply(status: StatusCode, value: &Value) -> Reply {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let mut response = Response::new(Full::new(Bytes::from(bytes)).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    secure(response, DATA_CSP)
}

fn error(status: StatusCode, message: &str) -> Reply {
    let mut response = json_reply(status, &json!({ "error": message }));
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"ShadowCode remote access\""),
        );
    }
    response
}

async fn handle(shared: &Arc<Shared>, peer: SocketAddr, request: Request<Incoming>) -> Reply {
    if shared.cancel.is_cancelled() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "Remote access is stopping");
    }
    if request.uri().scheme().is_some() || request.uri().authority().is_some() {
        return error(StatusCode::BAD_REQUEST, "Invalid request target");
    }
    let public_url = shared
        .manager
        .settings()
        .map(|s| s.public_url)
        .unwrap_or_default();
    if !known_host(request.headers(), &public_url) {
        return error(
            StatusCode::MISDIRECTED_REQUEST,
            "Unknown host name. Open the address from the pairing link, or enter this address as the public address in Settings › Remote access.",
        );
    }
    let path = request.uri().path().to_owned();
    if request.method() == Method::OPTIONS {
        // No CORS: cross-origin preflights are refused.
        return error(
            StatusCode::FORBIDDEN,
            "Cross-origin requests are not accepted",
        );
    }
    if path.starts_with("/api/") || path == "/api" {
        return api(shared, peer, request).await;
    }
    match (request.method().clone(), path.as_str()) {
        (Method::POST, "/_remote/pair") => pair(shared, peer, request).await,
        (Method::GET, "/_remote/session") => match authorize(shared, peer, request.headers()) {
            Ok(device) => json_reply(
                StatusCode::OK,
                &json!({
                    "device": {"id": device.id, "name": device.name},
                    "allow_terminals": shared.manager.allow_terminals(),
                    "version": crate::VERSION,
                }),
            ),
            Err(reply) => *reply,
        },
        (Method::GET, "/_remote/stream") => stream(shared, peer, request),
        (_, p) if p.starts_with("/_remote") => error(StatusCode::NOT_FOUND, "Not found"),
        (Method::GET | Method::HEAD, _) => {
            static_file(shared, &path, request.method() == Method::HEAD)
        }
        _ => error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

/// DNS rebinding: a web page can point its own name at this computer and
/// then talk to the server as its own origin (it still has no token, but it
/// could see the sign-in page and use up the failed-attempt budget of every
/// device behind a local proxy). Pairing links, and so every stored token,
/// use an IP address or the public address, so only those names are served:
/// IP addresses, `localhost` and `*.localhost` (always loopback), Tailscale
/// names (`*.ts.net`, not assignable by a web page) and the public address.
/// Requests without `Host` are not from a browser.
pub(super) fn known_host(headers: &HeaderMap, public_url: &str) -> bool {
    let hosts: Vec<_> = headers.get_all(header::HOST).iter().collect();
    let host = match hosts.as_slice() {
        [] => return true,
        [one] => match one.to_str() {
            Ok(host) => host.trim().to_ascii_lowercase(),
            Err(_) => return false,
        },
        _ => return false,
    };
    let name = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((v6, port)) if port.is_empty() || port.starts_with(':') => {
                return v6.parse::<std::net::Ipv6Addr>().is_ok();
            }
            _ => return false,
        }
    } else {
        match host.rsplit_once(':') {
            Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
            Some(_) => return false,
            None => host.as_str(),
        }
    };
    let public = reqwest::Url::parse(public_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
    !name.is_empty()
        && (name.parse::<std::net::Ipv4Addr>().is_ok()
            || name == "localhost"
            || name.ends_with(".localhost")
            || name.ends_with(".ts.net")
            || public.as_deref() == Some(name))
}

/// Browser requests must come from this server's own origin. Requests
/// without `Origin` (command-line clients, same-origin GETs) pass; the
/// token is still required.
pub(super) fn same_origin(headers: &HeaderMap, peer: SocketAddr) -> bool {
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|site| matches!(site, "cross-site" | "same-site"))
    {
        return false;
    }
    let origins: Vec<_> = headers.get_all(header::ORIGIN).iter().collect();
    let origin = match origins.as_slice() {
        [] => return true,
        [one] => match one.to_str() {
            Ok(origin) => origin,
            Err(_) => return false,
        },
        _ => return false,
    };
    let Some(authority) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    if authority.is_empty() || authority.contains('/') {
        return false;
    }
    let matches = |name: header::HeaderName| {
        let values: Vec<_> = headers.get_all(name).iter().collect();
        values.len() == 1
            && values[0]
                .to_str()
                .is_ok_and(|host| host.eq_ignore_ascii_case(authority))
    };
    // Behind a local reverse proxy (`tailscale serve`) the browser's host
    // arrives as X-Forwarded-Host; only a loopback peer may supply it.
    matches(header::HOST)
        || (peer.ip().is_loopback() && matches(header::HeaderName::from_static("x-forwarded-host")))
}

fn client_ip(peer: SocketAddr) -> std::net::IpAddr {
    peer.ip()
}

/// The paired device, or the (boxed) refusal to send.
fn authorize(shared: &Shared, peer: SocketAddr, headers: &HeaderMap) -> Result<Device, Box<Reply>> {
    let ip = client_ip(peer);
    if let Some(wait) = shared.manager.limiter().blocked(ip) {
        let mut reply = error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed attempts. Try again later.",
        );
        if let Ok(value) = HeaderValue::from_str(&wait.as_secs().max(1).to_string()) {
            reply.headers_mut().insert(header::RETRY_AFTER, value);
        }
        return Err(Box::new(reply));
    }
    let values: Vec<_> = headers.get_all(header::AUTHORIZATION).iter().collect();
    let device = match values.as_slice() {
        [one] => one
            .to_str()
            .ok()
            .and_then(auth::bearer)
            .and_then(|token| shared.manager.authenticate(token)),
        _ => None,
    };
    match device {
        Some(device) => {
            shared.manager.limiter().succeed(ip);
            Ok(device)
        }
        None => {
            shared.manager.limiter().fail(ip);
            Err(Box::new(error(
                StatusCode::UNAUTHORIZED,
                "This device is not paired. Open a new pairing link from the computer running ShadowCode.",
            )))
        }
    }
}

async fn read_body(body: Incoming, limit: usize) -> Result<Bytes, Box<Reply>> {
    match tokio::time::timeout(Duration::from_secs(30), Limited::new(body, limit).collect()).await {
        Err(_) => Err(Box::new(error(
            StatusCode::REQUEST_TIMEOUT,
            "The request body took too long",
        ))),
        Ok(Err(_)) => Err(Box::new(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "The request is too large",
        ))),
        Ok(Ok(collected)) => Ok(collected.to_bytes()),
    }
}

fn json_content(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
        })
}

async fn pair(shared: &Arc<Shared>, peer: SocketAddr, request: Request<Incoming>) -> Reply {
    if !same_origin(request.headers(), peer) {
        return error(
            StatusCode::FORBIDDEN,
            "Cross-origin requests are not accepted",
        );
    }
    let ip = client_ip(peer);
    if shared.manager.limiter().blocked(ip).is_some() {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed attempts. Try again later.",
        );
    }
    if !json_content(request.headers()) {
        return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Send JSON");
    }
    let body = match read_body(request.into_body(), PAIR_BODY_LIMIT).await {
        Ok(body) => body,
        Err(reply) => return *reply,
    };
    let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let code = value["code"].as_str().unwrap_or("");
    let name = value["name"].as_str().unwrap_or("");
    match shared.manager.redeem(code, name) {
        Ok((token, device)) => {
            shared.manager.limiter().succeed(ip);
            json_reply(
                StatusCode::OK,
                &json!({"token": token, "device": {"id": device.id, "name": device.name}}),
            )
        }
        Err(problem) => {
            shared.manager.limiter().fail(ip);
            error(StatusCode::UNAUTHORIZED, &format!("{problem:#}"))
        }
    }
}

fn valid_view(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn api(shared: &Arc<Shared>, peer: SocketAddr, request: Request<Incoming>) -> Reply {
    if !same_origin(request.headers(), peer) {
        return error(
            StatusCode::FORBIDDEN,
            "Cross-origin requests are not accepted",
        );
    }
    let device = match authorize(shared, peer, request.headers()) {
        Ok(device) => device,
        Err(reply) => return *reply,
    };
    let method = request.method().as_str().to_owned();
    if !matches!(method.as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
        return error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
    }
    let path = request
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_default();
    let view = request
        .headers()
        .get(VIEW_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| valid_view(v))
        .unwrap_or("default-view")
        .to_owned();
    let has_body = method != "GET";
    let typed = json_content(request.headers());
    let bytes = match read_body(request.into_body(), API_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(reply) => return *reply,
    };
    let body: Value = if !has_body || bytes.is_empty() {
        Value::Null
    } else if !typed {
        return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Send JSON");
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(body) => body,
            Err(_) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "The request body is not valid JSON",
                )
            }
        }
    };
    let access = policy::Access {
        allow_terminals: shared.manager.allow_terminals(),
    };
    if let Err(policy::Refusal(message)) =
        policy::check(&path, &body, &access, shared.manager.paths())
    {
        return error(StatusCode::FORBIDDEN, message);
    }
    let service = match shared.view(&device.id, &view) {
        Ok(service) => service,
        Err(problem) => return error(StatusCode::BAD_REQUEST, &format!("{problem:#}")),
    };
    let call = service.dispatch(ApiRequest { method, path, body });
    let result = tokio::select! {
        _ = shared.cancel.cancelled() => return error(StatusCode::SERVICE_UNAVAILABLE, "Remote access is stopping"),
        result = tokio::time::timeout(API_TIMEOUT, call) => result,
    };
    match result {
        Err(_) => error(StatusCode::GATEWAY_TIMEOUT, "The request took too long"),
        Ok(Ok(mut value)) => {
            policy::redact_response(&mut value);
            let bytes = match serde_json::to_vec(&value) {
                Ok(bytes) if bytes.len() <= RESPONSE_LIMIT => bytes,
                _ => {
                    return error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "The response is too large",
                    )
                }
            };
            let mut response = Response::new(Full::new(Bytes::from(bytes)).boxed());
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            secure(response, DATA_CSP)
        }
        Ok(Err(problem)) => {
            let mut message = Value::String(format!("{problem:#}"));
            policy::redact_response(&mut message);
            json_reply(StatusCode::BAD_REQUEST, &json!({"error": message}))
        }
    }
}

/// Server-Sent Events carrying the same wake-ups the desktop shell forwards:
/// `shadowcode:events` `{session_id, type}` and, when terminals are allowed,
/// `shadowcode:terminal` `{type, terminal_id}`. Never event payloads.
pub(super) fn stream_event(event: &Value, terminals: bool) -> Option<(&'static str, Value)> {
    let kind = event["type"].as_str().unwrap_or("");
    if kind.starts_with("terminal.") {
        let id = event["terminal_id"]
            .as_str()
            .filter(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))?;
        return terminals.then(|| {
            (
                "shadowcode:terminal",
                json!({"type": kind, "terminal_id": id}),
            )
        });
    }
    if kind.starts_with("view.") {
        return None;
    }
    Some((
        "shadowcode:events",
        json!({"session_id": event["session_id"], "type": event["type"]}),
    ))
}

fn sse(name: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {name}\ndata: {data}\n\n"))
}

struct EventBody {
    rx: mpsc::Receiver<Bytes>,
}
impl Body for EventBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.rx
            .poll_recv(cx)
            .map(|chunk| chunk.map(|bytes| Ok(Frame::data(bytes))))
    }
}

fn stream(shared: &Arc<Shared>, peer: SocketAddr, request: Request<Incoming>) -> Reply {
    if !same_origin(request.headers(), peer) {
        return error(
            StatusCode::FORBIDDEN,
            "Cross-origin requests are not accepted",
        );
    }
    let device = match authorize(shared, peer, request.headers()) {
        Ok(device) => device,
        Err(reply) => return *reply,
    };
    let Ok(permit) = shared.streams.clone().try_acquire_owned() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Too many open event streams",
        );
    };
    let (tx, rx) = mpsc::channel::<Bytes>(64);
    let mut events = shared.base.engine.subscribe();
    let (manager, cancel) = (shared.manager.clone(), shared.cancel.clone());
    tokio::spawn(async move {
        let _permit = permit;
        let send = |bytes: Bytes| {
            let tx = tx.clone();
            async move {
                matches!(
                    tokio::time::timeout(Duration::from_secs(10), tx.send(bytes)).await,
                    Ok(Ok(()))
                )
            }
        };
        // Reconnects re-read everything, like a lagged desktop stream.
        if !send(Bytes::from(format!(
            "retry: 3000\n\n{}",
            String::from_utf8_lossy(&sse("shadowcode:events", &json!({})))
        )))
        .await
        {
            return;
        }
        let mut keepalive = tokio::time::interval(KEEPALIVE);
        keepalive.tick().await;
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return,
                _ = keepalive.tick() => {
                    if !manager.paired(&device.id) {
                        return;
                    }
                    Bytes::from_static(b": keepalive\n\n")
                }
                received = events.recv() => match received {
                    Ok(event) => match stream_event(&event, manager.allow_terminals()) {
                        Some((name, data)) => sse(name, &data),
                        None => continue,
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        sse("shadowcode:events", &json!({}))
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
            };
            if !send(chunk).await {
                return;
            }
        }
    });
    let mut response = Response::new(EventBody { rx }.boxed());
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    secure(response, DATA_CSP)
}

fn static_file(shared: &Shared, path: &str, head: bool) -> Reply {
    let Some(key) = assets::asset_key(path) else {
        return error(StatusCode::NOT_FOUND, "Not found");
    };
    let Some(source) = shared.assets.as_ref() else {
        let mut response = Response::new(
            Full::new(Bytes::from_static(
                b"<!doctype html><title>ShadowCode</title><p>This build does not include the web interface. The API is available under /api/.</p>",
            ))
            .boxed(),
        );
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        return secure(response, HTML_CSP);
    };
    let Some(bytes) = source.get(&key) else {
        return error(StatusCode::NOT_FOUND, "Not found");
    };
    let length = bytes.len();
    let body = if head {
        Bytes::new()
    } else {
        Bytes::from(bytes.into_owned())
    };
    let mut response = Response::new(Full::new(body).boxed());
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(assets::content_type(&key)) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&length.to_string()) {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    if assets::immutable(&key) {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    let csp = if key.ends_with(".html") {
        HTML_CSP
    } else {
        DATA_CSP
    };
    secure(response, csp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn origins_must_match_this_server() {
        let lan: SocketAddr = "192.168.1.20:5000".parse().unwrap();
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        assert!(same_origin(&headers(&[("host", "192.168.1.5:7390")]), lan));
        assert!(same_origin(
            &headers(&[
                ("host", "192.168.1.5:7390"),
                ("origin", "http://192.168.1.5:7390")
            ]),
            lan
        ));
        for bad in [
            vec![
                ("host", "192.168.1.5:7390"),
                ("origin", "http://evil.example"),
            ],
            vec![("host", "192.168.1.5:7390"), ("origin", "null")],
            vec![
                ("host", "192.168.1.5:7390"),
                ("origin", "http://192.168.1.5:7391"),
            ],
            vec![
                ("host", "192.168.1.5:7390"),
                ("origin", "http://192.168.1.5:7390"),
                ("sec-fetch-site", "cross-site"),
            ],
            vec![
                ("host", "box.ts.net"),
                ("origin", "https://box.ts.net"),
                ("origin", "https://box.ts.net"),
            ],
        ] {
            assert!(!same_origin(&headers(&bad), lan), "{bad:?}");
        }
        // A local proxy forwards the browser's host.
        let proxied = headers(&[
            ("host", "127.0.0.1:7390"),
            ("x-forwarded-host", "box.tailnet.ts.net"),
            ("origin", "https://box.tailnet.ts.net"),
        ]);
        assert!(same_origin(&proxied, local));
        assert!(
            !same_origin(&proxied, lan),
            "only a loopback proxy may forward the host"
        );
    }

    #[test]
    fn rebound_host_names_are_refused() {
        for good in [
            "127.0.0.1:7390",
            "192.168.1.5:7390",
            "100.101.102.103:7390",
            "[::1]:7390",
            "[fd7a:115c:a1e0::1]:7390",
            "localhost:7390",
            "phone.localhost:7390",
            "box.tailnet.ts.net",
            "My-Box.example.org",
        ] {
            assert!(
                known_host(&headers(&[("host", good)]), "https://my-box.example.org"),
                "{good}"
            );
        }
        for bad in [
            "attacker.example:7390",
            "127.0.0.1.attacker.example:7390",
            "localhost.attacker.example",
            "my-box.example.org.attacker.example",
            "[::1:7390",
            "127.0.0.1:port",
            "",
        ] {
            assert!(
                !known_host(&headers(&[("host", bad)]), "https://my-box.example.org"),
                "{bad}"
            );
        }
        assert!(!known_host(
            &headers(&[("host", "127.0.0.1:7390"), ("host", "attacker.example")]),
            ""
        ));
        assert!(known_host(&HeaderMap::new(), ""), "not a browser");
    }

    #[test]
    fn stream_events_carry_wakeups_only() {
        let event = json!({"type":"agent.completed","session_id":"s1","payload":{"summary":"secret stuff"}});
        let (name, data) = stream_event(&event, false).unwrap();
        assert_eq!(name, "shadowcode:events");
        assert_eq!(data, json!({"session_id":"s1","type":"agent.completed"}));
        let id = "a".repeat(32);
        let terminal = json!({"type":"terminal.output","terminal_id":id});
        assert!(stream_event(&terminal, false).is_none());
        assert_eq!(
            stream_event(&terminal, true).unwrap().0,
            "shadowcode:terminal"
        );
        assert!(stream_event(
            &json!({"type":"terminal.output","terminal_id":"../x"}),
            true
        )
        .is_none());
        assert!(stream_event(&json!({"type":"view.disconnected"}), true).is_none());
    }

    #[test]
    fn view_ids_are_bounded() {
        assert!(valid_view("tab-0123456789"));
        assert!(!valid_view("short"));
        assert!(!valid_view("has space in it"));
        assert!(!valid_view(&"x".repeat(65)));
    }
}
