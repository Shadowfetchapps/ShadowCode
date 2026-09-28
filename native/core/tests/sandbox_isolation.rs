//! Live checks of the shell sandbox: the real bubblewrap profile, the
//! network allow-list proxy and the Landlock fallback. A test that needs a
//! layer this machine cannot provide (no bubblewrap, no unprivileged user
//! namespaces, no Landlock, no curl) prints why and returns.
use serde_json::json;
use shadowcode_core::{
    config::ShellNetwork,
    process::{self, ProcessResult, ProcessSpec},
    sandbox::{self, PreparedShell, ShellPolicy},
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

fn policy(network: ShellNetwork, allow: &[String], require: bool) -> ShellPolicy {
    ShellPolicy {
        network,
        allow: sandbox::proxy::parse_list(allow).unwrap(),
        allow_text: allow.to_vec(),
        home_binds: sandbox::DEFAULT_HOME_BINDS
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        require,
        landlock: true,
    }
}

async fn run(prepared: PreparedShell, cwd: &Path) -> anyhow::Result<ProcessResult> {
    let spec = ProcessSpec {
        program: prepared.program,
        args: prepared.args,
        cwd: cwd.to_path_buf(),
        timeout: Duration::from_secs(60),
        output_limit: 64_000,
        env: Default::default(),
        child: prepared.child,
    };
    let result = process::run(spec, CancellationToken::new(), None).await;
    if let Some(scratch) = prepared.scratch {
        let _ = sandbox::discard_scratch(&scratch);
    }
    result
}

/// A folder in the real home, outside every allowed toolchain folder.
fn home_probe() -> Option<(tempfile::TempDir, PathBuf)> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let dir = tempfile::Builder::new()
        .prefix(".shadowcode-sandbox-probe-")
        .tempdir_in(&home)
        .ok()?;
    let file = dir.path().join("outside-the-sandbox");
    fs::write(&file, "visible only outside").ok()?;
    Some((dir, file))
}

#[tokio::test]
async fn bubblewrap_hides_home_and_keeps_the_project_writable() {
    let Some((probe_dir, probe)) = home_probe() else {
        eprintln!("skipped: no writable home folder");
        return;
    };
    let ws = tempfile::tempdir().unwrap();
    let marker = format!("sandbox-home-write-{}", shadowcode_core::id());
    // Existence checks only: nothing in the real home is read.
    let script = format!(
        r#"test ! -e "{probe}" || exit 11
test ! -e "$HOME/.ssh" || exit 12
test ! -e "$HOME/.config" || exit 13
test ! -e "$HOME/.local/share" || exit 14
touch "$HOME/{marker}" || exit 15
printf ok > written.txt || exit 16
"#,
        probe = probe.display()
    );
    let prepared = match sandbox::prepare_shell(
        ws.path(),
        ws.path(),
        &script,
        &policy(ShellNetwork::Off, &[], true),
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("skipped: {error:#}");
            return;
        }
    };
    assert_eq!(prepared.note["mode"], "bubblewrap");
    assert_eq!(prepared.note["network"], "off");
    let result = run(prepared, ws.path()).await.unwrap();
    assert!(
        result.ok,
        "exit {}: {}{}",
        result.exit_code, result.stdout, result.stderr
    );
    assert_eq!(
        fs::read_to_string(ws.path().join("written.txt")).unwrap(),
        "ok"
    );
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    assert!(
        !home.join(&marker).exists(),
        "home writes stay in the sandbox"
    );
    drop(probe_dir);
}

/// A one-response HTTP server on the host's loopback.
async fn upstream() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                let body = "hello-from-upstream";
                let _ = stream
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
    port
}

#[tokio::test]
async fn allowlist_proxy_is_the_only_way_out() {
    if sandbox::which("curl").is_none() {
        eprintln!("skipped: curl is not installed");
        return;
    }
    let port = upstream().await;
    let ws = tempfile::tempdir().unwrap();
    let script = format!(
        r#"curl -s -m 5 http://127.0.0.1:{port}/via-proxy > via-proxy.txt
curl -s -m 5 --noproxy '*' http://127.0.0.1:{port}/direct > /dev/null; echo "$?" > direct.code
curl -s -m 5 -o /dev/null -w '%{{http_code}}' http://blocked.invalid/ > blocked.code
exit 0
"#
    );
    let allow = vec![format!("127.0.0.1:{port}")];
    let prepared = match sandbox::prepare_shell(
        ws.path(),
        ws.path(),
        &script,
        &policy(ShellNetwork::Allowlist, &allow, true),
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("skipped: {error:#}");
            return;
        }
    };
    assert_eq!(prepared.note["network"], "allowlist");
    assert_eq!(prepared.note["allow"], json!(allow));
    let gate = prepared.gate.clone().expect("allow-list gate");
    let result = match run(prepared, ws.path()).await {
        Ok(result) => result,
        Err(error) => {
            eprintln!("skipped: private network namespaces are unavailable: {error:#}");
            return;
        }
    };
    assert!(result.ok, "{}{}", result.stdout, result.stderr);
    let read = |name: &str| fs::read_to_string(ws.path().join(name)).unwrap_or_default();
    assert_eq!(read("via-proxy.txt"), "hello-from-upstream");
    assert_ne!(
        read("direct.code").trim(),
        "0",
        "direct connections have no route"
    );
    assert_eq!(read("blocked.code").trim(), "403");
    let report = gate.report();
    assert!(report["reached"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == &json!(format!("127.0.0.1:{port}"))));
    assert!(report["blocked"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "blocked.invalid:80"));
}

#[tokio::test]
async fn network_off_and_on_inside_bubblewrap() {
    if sandbox::which("curl").is_none() {
        eprintln!("skipped: curl is not installed");
        return;
    }
    let port = upstream().await;
    let ws = tempfile::tempdir().unwrap();
    let script = format!(
        "curl -s -m 5 --noproxy '*' http://127.0.0.1:{port}/ > out.txt; echo \"$?\" > code.txt"
    );
    for (network, reachable) in [(ShellNetwork::Off, false), (ShellNetwork::On, true)] {
        let prepared = match sandbox::prepare_shell(
            ws.path(),
            ws.path(),
            &script,
            &policy(network, &[], true),
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                eprintln!("skipped: {error:#}");
                return;
            }
        };
        run(prepared, ws.path()).await.unwrap();
        let code = fs::read_to_string(ws.path().join("code.txt")).unwrap();
        assert_eq!(
            code.trim() == "0",
            reachable,
            "{network:?}: curl exit {code}"
        );
    }
}

#[test]
fn landlock_limits_commands_when_bubblewrap_is_missing() {
    // PATH is process-global: hide bubblewrap in a child test process only.
    if std::env::var_os("SHADOWCODE_LANDLOCK_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "landlock_limits_commands_when_bubblewrap_is_missing",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("SHADOWCODE_LANDLOCK_CHILD", "1")
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
    let Some((probe_dir, probe)) = home_probe() else {
        eprintln!("skipped: no writable home folder");
        return;
    };
    let ws = tempfile::tempdir().unwrap();
    // A named Unix socket outside the workspace and temporary folders (like the
    // D-Bus session bus or ShadowCode's own control socket under /run/user):
    // reachable only where Landlock grants ResolveUnix, which the fallback
    // grants only under the workspace and temporary folders.
    let socket_path = probe_dir.path().join("outside.sock");
    let abi = shadowcode_core::sandbox::status(&shadowcode_core::config::Config::default())
        ["landlock_abi"]
        .as_i64()
        .unwrap_or(0);
    let check_socket = abi >= 9 && Path::new("/usr/bin/python3").exists();
    let _listener =
        check_socket.then(|| std::os::unix::net::UnixListener::bind(&socket_path).unwrap());
    let socket_probe = if check_socket {
        format!(
            "python3 -c \"import socket,sys; s=socket.socket(socket.AF_UNIX); \
sys.exit(0 if s.connect_ex('{}')==0 else 1)\" 2>/dev/null && exit 14\n",
            socket_path.display()
        )
    } else {
        String::new()
    };
    std::env::set_var("PATH", "");
    let script = format!(
        r#"PATH=/usr/bin:/bin
cat "{probe}" > /dev/null 2>&1 && exit 11
touch "{dir}/created-by-sandbox" 2>/dev/null && exit 12
{socket_probe}printf ok > written.txt || exit 13
exit 0
"#,
        probe = probe.display(),
        dir = probe_dir.path().display()
    );
    let prepared = sandbox::prepare_shell(
        ws.path(),
        ws.path(),
        &script,
        &policy(ShellNetwork::Off, &[], false),
    )
    .unwrap();
    if prepared.note["mode"] != "landlock" {
        eprintln!("skipped: this kernel has no Landlock");
        return;
    }
    assert!(prepared.warning.as_deref().unwrap().contains("Landlock"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let result = runtime.block_on(run(prepared, ws.path())).unwrap();
    assert!(
        result.ok,
        "exit {}: {}{}",
        result.exit_code, result.stdout, result.stderr
    );
    assert_eq!(
        fs::read_to_string(ws.path().join("written.txt")).unwrap(),
        "ok"
    );
    assert!(!probe_dir.path().join("created-by-sandbox").exists());
}
