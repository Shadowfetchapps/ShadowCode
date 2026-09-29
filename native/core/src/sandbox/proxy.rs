//! Host allow-list for sandboxed shell commands (`network.shell: allowlist`).
//!
//! The command runs in a network namespace whose only interface is loopback.
//! A listening socket created inside that namespace is handed to this
//! process, which serves it as an HTTP proxy: `CONNECT host:port` (HTTPS and
//! other TLS) and absolute-form `http://` requests are forwarded only when the
//! host is on the list. Everything else (raw TCP, UDP, DNS) has no route out.
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// Port the proxy listens on inside the command's private network namespace.
pub const PROXY_PORT: u16 = 3128;
const MAX_HEAD: usize = 64 * 1024;
const MAX_CONNECTIONS: usize = 64;
const MAX_ENTRIES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowEntry {
    /// Lower-case host or IP literal, without brackets.
    pub host: String,
    /// `*.example.com`: the domain and every subdomain.
    pub wildcard: bool,
    /// `None` means the web ports 80 and 443.
    pub port: Option<u16>,
}

impl AllowEntry {
    pub fn matches(&self, host: &str, port: u16) -> bool {
        let host = normalize_host(host);
        let port_ok = match self.port {
            Some(p) => p == port,
            None => port == 80 || port == 443,
        };
        let host_ok = if self.wildcard {
            host == self.host || host.ends_with(&format!(".{}", self.host))
        } else {
            host == self.host
        };
        port_ok && host_ok
    }
    fn literal(&self) -> bool {
        !self.wildcard && (self.host == "localhost" || self.host.parse::<IpAddr>().is_ok())
    }
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// `example.com`, `*.example.com`, `example.com:8443`, `10.0.0.5:5000`,
/// `[::1]:3000`. A URL prefix (`https://`) and trailing slash are accepted.
pub fn parse_entry(entry: &str) -> Result<AllowEntry> {
    let entry = entry.trim();
    let bare = entry
        .strip_prefix("http://")
        .or_else(|| entry.strip_prefix("https://"))
        .unwrap_or(entry)
        .trim_end_matches('/');
    ensure!(
        !bare.is_empty() && bare.len() <= 260 && !bare.contains(['/', '@', '?', '#', ' ']),
        "Allowed hosts look like example.com, *.example.com or example.com:8443"
    );
    let (host, port) = if let Some(rest) = bare.strip_prefix('[') {
        let (host, rest) = rest.split_once(']').context("Unclosed IPv6 bracket")?;
        let port = match rest {
            "" => None,
            rest => Some(rest.strip_prefix(':').context("Expected :port after ]")?),
        };
        (host.to_owned(), port)
    } else {
        match bare.rsplit_once(':') {
            Some((host, port)) => {
                ensure!(!host.contains(':'), "Write IPv6 hosts as [::1]:port");
                (host.to_owned(), Some(port))
            }
            None => (bare.to_owned(), None),
        }
    };
    let port = port
        .map(|p| {
            p.parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .context("Port must be between 1 and 65535")
        })
        .transpose()?;
    let (wildcard, host) = match host.strip_prefix("*.") {
        Some(rest) => (true, rest.to_owned()),
        None => (false, host),
    };
    let host = normalize_host(&host);
    ensure!(
        !host.is_empty()
            && !host.contains('*')
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_')),
        "Invalid host '{entry}'"
    );
    ensure!(
        !wildcard || host.contains('.'),
        "A wildcard needs a domain with a dot, for example *.example.com"
    );
    Ok(AllowEntry {
        host,
        wildcard,
        port,
    })
}

pub fn parse_list(entries: &[String]) -> Result<Vec<AllowEntry>> {
    ensure!(
        entries.len() <= MAX_ENTRIES,
        "network.allow holds at most {MAX_ENTRIES} hosts"
    );
    entries
        .iter()
        .map(|e| parse_entry(e).with_context(|| format!("Invalid network.allow entry '{e}'")))
        .collect()
}

pub fn allowed(entries: &[AllowEntry], host: &str, port: u16) -> bool {
    entries.iter().any(|e| e.matches(host, port))
}

/// The allow-list plus what the proxy decided, for the tool result.
#[derive(Debug, Default)]
pub struct Gate {
    pub allow: Vec<AllowEntry>,
    decisions: Mutex<(Vec<String>, Vec<String>)>,
}

impl Gate {
    pub fn new(allow: Vec<AllowEntry>) -> Arc<Self> {
        Arc::new(Self {
            allow,
            decisions: Mutex::default(),
        })
    }
    fn note(&self, allowed: bool, target: String) {
        if let Ok(mut d) = self.decisions.lock() {
            let list = if allowed { &mut d.0 } else { &mut d.1 };
            if list.len() < 50 && !list.contains(&target) {
                list.push(target);
            }
        }
    }
    pub fn report(&self) -> Value {
        let (allowed, blocked) = self.decisions.lock().map(|d| d.clone()).unwrap_or_default();
        json!({"reached":allowed,"blocked":blocked})
    }
}

/// Serve the proxy until the returned task is aborted.
pub async fn serve(listener: TcpListener, gate: Arc<Gate>) {
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        };
        let Ok(permit) = slots.clone().acquire_owned().await else {
            return;
        };
        let gate = gate.clone();
        tokio::spawn(async move {
            let _ = handle(stream, &gate).await;
            drop(permit);
        });
    }
}

struct Head {
    connect: bool,
    host: String,
    port: u16,
    /// What to send upstream first (rewritten request head for plain HTTP).
    forward: Vec<u8>,
}

/// Parse one proxy request head (without the body that follows it).
fn parse_head(head: &[u8]) -> Result<Head> {
    let text = std::str::from_utf8(head).context("Request head is not UTF-8")?;
    let mut lines = text.split("\r\n");
    let request = lines.next().context("Empty request")?;
    let mut parts = request.split(' ');
    let (method, target, version) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    );
    ensure!(
        !method.is_empty() && version.starts_with("HTTP/1."),
        "Unsupported proxy request"
    );
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_host_port(target, None)?;
        return Ok(Head {
            connect: true,
            host,
            port,
            forward: Vec::new(),
        });
    }
    let rest = target
        .strip_prefix("http://")
        .context("Only http:// URLs and CONNECT are supported by the allow-list proxy")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    ensure!(
        !authority.contains('@'),
        "Credentials in proxy URLs are not supported"
    );
    let (host, port) = split_host_port(authority, Some(80))?;
    let mut forward = format!("{method} {path} {version}\r\n");
    for line in lines {
        let name = line.split(':').next().unwrap_or("").trim();
        if name.eq_ignore_ascii_case("proxy-connection")
            || name.eq_ignore_ascii_case("proxy-authorization")
        {
            continue;
        }
        forward.push_str(line);
        forward.push_str("\r\n");
    }
    // `lines` ended with the two empty strings of the terminating CRLFCRLF.
    while forward.ends_with("\r\n\r\n\r\n") {
        forward.truncate(forward.len() - 2);
    }
    Ok(Head {
        connect: false,
        host,
        port,
        forward: forward.into_bytes(),
    })
}

fn split_host_port(authority: &str, default: Option<u16>) -> Result<(String, u16)> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, rest) = rest.split_once(']').context("Unclosed IPv6 bracket")?;
        (host, rest.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    let port = match port {
        Some(p) => p
            .parse()
            .ok()
            .filter(|p: &u16| *p > 0)
            .context("Bad port")?,
        None => default.context("CONNECT needs host:port")?,
    };
    let host = normalize_host(host);
    ensure!(!host.is_empty(), "Missing host");
    Ok((host, port))
}

/// Refuse loopback, link-local (cloud metadata) and unspecified addresses
/// unless the allow-list names that literal address or `localhost`.
fn usable(addr: &SocketAddr, literal: bool) -> bool {
    if literal {
        return true;
    }
    let usable_v4 = |ip: Ipv4Addr| !(ip.is_loopback() || ip.is_link_local() || ip.is_unspecified());
    match addr.ip() {
        IpAddr::V4(ip) => usable_v4(ip),
        IpAddr::V6(ip) => {
            // An IPv4-mapped address (`::ffff:127.0.0.1`, `::ffff:169.254.169.254`)
            // reaches the embedded IPv4 host, so judge it as that address.
            if let Some(v4) = ip.to_ipv4_mapped() {
                return usable_v4(v4);
            }
            !(ip.is_loopback() || ip.is_unspecified() || (ip.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

async fn deny(stream: &mut TcpStream, status: &str, text: &str) -> Result<()> {
    let body = format!("ShadowCode sandbox: {text}\n");
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(reply.as_bytes()).await?;
    Ok(())
}

async fn handle(mut client: TcpStream, gate: &Gate) -> Result<()> {
    let mut buffer = Vec::with_capacity(4096);
    let end = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let mut chunk = [0u8; 4096];
            let n = client.read(&mut chunk).await?;
            ensure!(n > 0, "Client closed before sending a request");
            buffer.extend_from_slice(&chunk[..n]);
            if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                return Ok(end + 4);
            }
            ensure!(buffer.len() <= MAX_HEAD, "Request head too large");
        }
    })
    .await
    .context("Timed out reading the proxy request")??;
    let head = match parse_head(&buffer[..end]) {
        Ok(head) => head,
        Err(error) => {
            deny(&mut client, "400 Bad Request", &format!("{error}")).await?;
            return Ok(());
        }
    };
    let target = format!("{}:{}", head.host, head.port);
    let Some(entry) = gate.allow.iter().find(|e| e.matches(&head.host, head.port)) else {
        gate.note(false, target.clone());
        deny(
            &mut client,
            "403 Forbidden",
            &format!(
                "{target} is not in the allowed hosts list (Settings > Permissions & network)."
            ),
        )
        .await?;
        return Ok(());
    };
    let literal = entry.literal();
    let addresses: Vec<SocketAddr> = match tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::lookup_host((head.host.as_str(), head.port)),
    )
    .await
    {
        Ok(Ok(found)) => found.filter(|a| usable(a, literal)).collect(),
        _ => Vec::new(),
    };
    let mut upstream = None;
    for address in addresses.iter().take(4) {
        if let Ok(Ok(stream)) =
            tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(address)).await
        {
            upstream = Some(stream);
            break;
        }
    }
    let Some(mut upstream) = upstream else {
        gate.note(false, target.clone());
        let why = if addresses.is_empty() {
            "does not resolve to a public address"
        } else {
            "could not be reached"
        };
        deny(&mut client, "502 Bad Gateway", &format!("{target} {why}.")).await?;
        return Ok(());
    };
    gate.note(true, target);
    if head.connect {
        client
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await?;
    } else {
        upstream.write_all(&head.forward).await?;
    }
    if buffer.len() > end {
        upstream.write_all(&buffer[end..]).await?;
    }
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

/// Accept connections on `listener` in the background; aborting the handle
/// stops the proxy (open tunnels end when their sockets drop).
pub fn spawn(
    listener: std::net::TcpListener,
    gate: Arc<Gate>,
) -> Result<tokio::task::JoinHandle<()>> {
    listener.set_nonblocking(true)?;
    let listener = TcpListener::from_std(listener)?;
    Ok(tokio::spawn(serve(listener, gate)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_parse_and_match() {
        let list = parse_list(&[
            "crates.io".into(),
            "*.githubusercontent.com".into(),
            "localhost:3000".into(),
            "https://Registry.NPMJS.org/".into(),
            "[::1]:8080".into(),
        ])
        .unwrap();
        assert!(allowed(&list, "crates.io", 443));
        assert!(allowed(&list, "crates.io", 80));
        assert!(!allowed(&list, "crates.io", 22));
        assert!(!allowed(&list, "static.crates.io", 443));
        assert!(allowed(&list, "raw.githubusercontent.com", 443));
        assert!(allowed(&list, "githubusercontent.com", 443));
        assert!(!allowed(&list, "evilgithubusercontent.com", 443));
        assert!(allowed(&list, "localhost", 3000));
        assert!(!allowed(&list, "localhost", 443));
        assert!(allowed(&list, "registry.npmjs.org.", 443));
        assert!(allowed(&list, "[::1]", 8080));
        for bad in [
            "",
            "a b",
            "user@host",
            "*.com",
            "host:0",
            "host:99999",
            "ex*ample.com",
        ] {
            assert!(parse_entry(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn heads_parse_connect_and_absolute_http() {
        let head = parse_head(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n")
            .unwrap();
        assert!(head.connect);
        assert_eq!((head.host.as_str(), head.port), ("example.com", 443));
        let head = parse_head(
            b"GET http://Example.com/a?b=1 HTTP/1.1\r\nHost: example.com\r\nProxy-Connection: keep-alive\r\n\r\n",
        )
        .unwrap();
        assert!(!head.connect);
        assert_eq!((head.host.as_str(), head.port), ("example.com", 80));
        let text = String::from_utf8(head.forward).unwrap();
        assert!(text.starts_with("GET /a?b=1 HTTP/1.1\r\n"));
        assert!(!text.to_ascii_lowercase().contains("proxy-connection"));
        assert!(text.ends_with("\r\n\r\n") && !text.ends_with("\r\n\r\n\r\n"));
        assert!(parse_head(b"GET /relative HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_head(b"GET ftp://x/ HTTP/1.1\r\n\r\n").is_err());
    }

    #[test]
    fn metadata_and_loopback_need_a_literal_entry() {
        let meta: SocketAddr = "169.254.169.254:80".parse().unwrap();
        let lo: SocketAddr = "127.0.0.1:443".parse().unwrap();
        let public: SocketAddr = "93.184.216.34:443".parse().unwrap();
        assert!(!usable(&meta, false));
        assert!(!usable(&lo, false));
        assert!(usable(&public, false));
        assert!(usable(&lo, true));
        // IPv4-mapped IPv6 forms reach the embedded host and are judged as it.
        let mapped_meta: SocketAddr = "[::ffff:169.254.169.254]:80".parse().unwrap();
        let mapped_lo: SocketAddr = "[::ffff:127.0.0.1]:443".parse().unwrap();
        let mapped_public: SocketAddr = "[::ffff:93.184.216.34]:443".parse().unwrap();
        assert!(!usable(&mapped_meta, false));
        assert!(!usable(&mapped_lo, false));
        assert!(usable(&mapped_public, false));
    }

    async fn ask(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reply = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut reply)).await;
        String::from_utf8_lossy(&reply).into_owned()
    }

    #[tokio::test]
    async fn proxy_forwards_allowed_hosts_and_refuses_others() {
        // Upstream: a tiny HTTP server on loopback.
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up_port = upstream.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = upstream.accept().await {
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let first = String::from_utf8_lossy(&buf[..n])
                        .lines()
                        .next()
                        .unwrap_or("")
                        .to_owned();
                    let body = format!("upstream saw: {first}");
                    let _ = s
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                });
            }
        });
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let gate = Gate::new(parse_list(&[format!("127.0.0.1:{up_port}")]).unwrap());
        let task = spawn(listener, gate.clone()).unwrap();

        let ok = ask(
            port,
            &format!("GET http://127.0.0.1:{up_port}/x HTTP/1.1\r\nHost: a\r\n\r\n"),
        )
        .await;
        assert!(ok.contains("upstream saw: GET /x HTTP/1.1"), "{ok}");

        let tunnel = ask(
            port,
            &format!("CONNECT 127.0.0.1:{up_port} HTTP/1.1\r\n\r\nGET /t HTTP/1.1\r\n\r\n"),
        )
        .await;
        assert!(
            tunnel.starts_with("HTTP/1.1 200 Connection established"),
            "{tunnel}"
        );
        assert!(tunnel.contains("upstream saw: GET /t"), "{tunnel}");

        let denied = ask(port, "CONNECT example.com:443 HTTP/1.1\r\n\r\n").await;
        assert!(denied.starts_with("HTTP/1.1 403"), "{denied}");
        assert!(denied.contains("example.com:443"));
        // Loopback is reachable only because the entry is a literal address.
        let other_port = ask(
            port,
            &format!(
                "CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n",
                up_port.wrapping_add(1).max(1)
            ),
        )
        .await;
        assert!(other_port.starts_with("HTTP/1.1 403"), "{other_port}");

        let report = gate.report();
        assert_eq!(report["reached"][0], format!("127.0.0.1:{up_port}"));
        assert!(report["blocked"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b == "example.com:443"));
        task.abort();
    }
}
