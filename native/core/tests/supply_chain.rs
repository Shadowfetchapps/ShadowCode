//! New packages are looked up on their registry (a local fake here) before
//! the user approves installing them, and the approval card says what was
//! found.
use serde_json::json;
use shadowcode_core::{
    approvals::{Answer, ApprovalHub},
    config::{Config, PermissionMode},
    events::TaskEvents,
    models::ToolCall,
    store::Store,
    supply_chain::{self, Ecosystem, Package, Status},
    tools::ToolExecutor,
    workspace::Workspace,
};
use std::{
    fs,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

/// A registry answering by path: npm packuments, PyPI JSON, 404 otherwise.
async fn registry(hits: Arc<AtomicUsize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let recent = {
        let days = (shadowcode_core::now() / 86_400.0).floor() as i64 - 2;
        // Days since the epoch → a civil date (the reverse of the engine's).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!("{y:04}-{m:02}-{d:02}T08:00:00.000Z")
    };
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let hits = hits.clone();
            let recent = recent.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0; 8192];
                let count = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..count]);
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                hits.fetch_add(1, Ordering::SeqCst);
                let (status, body) = match path.as_str() {
                    "/left-pad" => (200, json!({"time":{"created":"2016-03-23T03:23:17.453Z"}})),
                    "/fresh-thing" => (200, json!({"time":{"created":recent}})),
                    "/@types%2Fnode" => {
                        (200, json!({"time":{"created":"2016-05-17T18:23:26.000Z"}}))
                    }
                    "/pypi/requests/json" => (
                        200,
                        json!({"releases":{"0.2.0":[{"upload_time_iso_8601":"2011-02-14T12:00:00.000000Z"}],"2.32.3":[{"upload_time_iso_8601":"2024-05-29T12:00:00.000000Z"}]}}),
                    ),
                    _ => (404, json!({"error":"Not found"})),
                };
                let body = body.to_string();
                let reason = if status == 200 { "OK" } else { "Not Found" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    base
}

fn package(ecosystem: Ecosystem, name: &str) -> Package {
    Package {
        ecosystem,
        name: name.into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn new_packages_are_looked_up_and_shown_on_the_approval_card() {
    let hits = Arc::new(AtomicUsize::new(0));
    let base = registry(hits.clone()).await;
    // One test sets the registries for this whole test binary.
    std::env::set_var("SHADOWCODE_REGISTRY_NPM", &base);
    std::env::set_var("SHADOWCODE_REGISTRY_PYPI", &base);
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&root.path().join("db")).unwrap());

    let packages = [
        package(Ecosystem::Npm, "left-pad"),
        package(Ecosystem::Npm, "fresh-thing"),
        package(Ecosystem::Npm, "lodahs"),
        package(Ecosystem::Npm, "@types/node"),
        package(Ecosystem::PyPI, "requests"),
    ];
    let verdicts = supply_chain::check(&packages, false, Some(&store)).await;
    assert!(
        matches!(verdicts[0].status, Status::Known { age_days: Some(age) } if age > 3000.0),
        "{:?}",
        verdicts[0]
    );
    assert!(
        matches!(verdicts[1].status, Status::New { age_days } if (1.0..4.0).contains(&age_days)),
        "{:?}",
        verdicts[1]
    );
    assert_eq!(verdicts[2].status, Status::Missing);
    assert_eq!(verdicts[2].lookalike.as_deref(), Some("lodash"));
    assert!(
        matches!(verdicts[3].status, Status::Known { .. }),
        "{:?}",
        verdicts[3]
    );
    assert!(
        matches!(verdicts[4].status, Status::Known { age_days: Some(age) } if age > 4000.0),
        "{:?}",
        verdicts[4]
    );
    let section = supply_chain::section(&verdicts).unwrap();
    assert_eq!(section["level"], "danger");
    assert_eq!(section["title"], "New packages");

    // Answers are cached for a day: the same lookups don't ask again.
    let asked = hits.load(Ordering::SeqCst);
    supply_chain::check(&packages[..3], false, Some(&store)).await;
    assert_eq!(hits.load(Ordering::SeqCst), asked);
    // Offline, nothing is asked and nothing is blocked.
    let offline = supply_chain::check(
        &[package(Ecosystem::Npm, "brand-new-x")],
        true,
        Some(&store),
    )
    .await;
    assert_eq!(offline[0].status, Status::Unchecked("offline".into()));
    assert_eq!(hits.load(Ordering::SeqCst), asked);

    // On the card of an install command.
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let session = store.create_session(&project, "mock", "").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let task = store.create_task(&session, "t").unwrap();
    let (sender, _) = tokio::sync::broadcast::channel(100);
    let mut config = Config::default();
    config.permissions.mode = PermissionMode::Ask;
    config.permissions.network = true;
    let tools = Arc::new(
        ToolExecutor::new(
            Arc::new(Workspace::open(&project).unwrap()),
            config,
            ApprovalHub::default(),
            TaskEvents {
                store: store.clone(),
                session_id: session.clone(),
                task_id: task,
                sender,
            },
            CancellationToken::new(),
        )
        .unwrap(),
    );
    let running = {
        let tools = tools.clone();
        tokio::spawn(async move {
            tools
                .execute(ToolCall {
                    id: shadowcode_core::id(),
                    name: "exec".into(),
                    arguments: json!({"command":"npm install lodahs"}),
                })
                .await
                .unwrap()
        })
    };
    let card = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(card) = tools.approvals.list(None).pop() {
                break card;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let check = &card.assessment["checks"][0];
    assert_eq!(check["title"], "New package", "{}", card.assessment);
    assert_eq!(check["level"], "danger");
    let item = check["items"][0].as_str().unwrap();
    assert!(item.contains("not found on npm"), "{item}");
    assert!(item.contains("“lodash”"), "{item}");
    tools
        .approvals
        .answer(&card.id, &session, Answer::deny())
        .unwrap();
    assert!(!running.await.unwrap().success);
}
