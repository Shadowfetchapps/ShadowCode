use super::*;
use std::sync::Mutex as StdMutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const GB: u64 = 1_000_000_000;

/// The llama.cpp commit whose `architectures.txt` the catalog was checked
/// against, and the architectures from that list the catalog uses. Moving
/// the pin means checking the catalog again (see docs/LOCAL_MODELS.md).
const VERIFIED_PIN: &str = "18f9f7bef960b76b693d8dcbb33cbbd6148c1631";
const PIN_ARCHITECTURES: &[&str] = &["gemma4", "gpt-oss", "granite", "qwen35moe"];

#[test]
fn catalog_is_pinned_licensed_and_ordered() {
    let hex = |s: &str, len: usize| {
        s.len() == len
            && s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    };
    let mut ids = std::collections::HashSet::new();
    let mut previous = 0;
    for model in CATALOG {
        assert!(ids.insert(model.id), "duplicate id {}", model.id);
        assert!(
            hex(model.commit, 40),
            "{} is not pinned to a commit",
            model.id
        );
        assert!(hex(model.sha256, 64), "{} has no SHA-256", model.id);
        let (owner, name) = model.repo.split_once('/').expect("owner/name");
        assert!(!owner.is_empty() && !name.is_empty() && !name.contains('/'));
        assert!(
            model.file.ends_with(".gguf") && !model.file.contains(['/', '\\']),
            "{}",
            model.file
        );
        assert_eq!(
            model.url(),
            format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                model.repo, model.commit, model.file
            )
        );
        assert!(model.source_url().contains(model.commit));
        // Permissive licenses only; Gemma 4 is Apache-2.0 (unlike Gemma 1-3).
        assert!(
            ["Apache-2.0", "MIT"].contains(&model.license),
            "{} is {}",
            model.id,
            model.license
        );
        assert!(model.license_url.starts_with("https://"));
        assert!(
            PIN_ARCHITECTURES.contains(&model.architecture),
            "{} needs {} in the runtime",
            model.id,
            model.architecture
        );
        assert!(model.memory_bytes > model.bytes, "{}", model.id);
        assert!(model.min_memory_bytes <= model.memory_bytes, "{}", model.id);
        assert!(model.min_memory_bytes > model.bytes, "{}", model.id);
        assert!(model.bytes > previous, "the catalog runs small to large");
        previous = model.bytes;
        assert!(!model.summary.is_empty() && model.summary.len() < 120);
        assert_eq!(entry(model.id).map(|m| m.id), Some(model.id));
    }
    assert!((3..=8).contains(&CATALOG.len()), "keep the list short");
    assert!(CATALOG[0].bytes < 3 * GB && CATALOG[0].cpu_friendly);
    assert!(entry("llama-3.2-3b").is_none());
}

#[test]
fn catalog_was_checked_against_the_pinned_runtime() {
    let pin = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/llama.cpp.pin"),
    )
    .unwrap();
    assert!(
        pin.lines().any(|l| l == format!("commit={VERIFIED_PIN}")),
        "tools/llama.cpp.pin moved: check that its architectures.txt still lists {PIN_ARCHITECTURES:?}, then update VERIFIED_PIN"
    );
}

#[test]
fn recommendation_follows_the_hardware() {
    let gib = |n: u64| n * GIB;
    let pick = |ram: u64, vram: Option<u64>| recommend(ram, vram).map(|(m, f)| (m.id, f));
    // This developer machine: 16 GB graphics card, 62 GB RAM.
    assert_eq!(
        pick(gib(62), Some(16_557 * MIB)),
        Some(("gpt-oss-20b", Fit::Gpu))
    );
    assert_eq!(
        pick(gib(64), Some(gib(24))),
        Some(("qwen3.6-35b-a3b", Fit::Gpu))
    );
    assert_eq!(
        pick(gib(32), Some(gib(12))),
        Some(("gemma-4-12b", Fit::Gpu))
    );
    assert_eq!(pick(gib(16), Some(gib(8))), Some(("gemma-4-e4b", Fit::Gpu)));
    // No graphics card: only models that run well on a processor, with room
    // left for other apps.
    assert_eq!(pick(gib(32), None), Some(("qwen3.6-35b-a3b", Fit::Cpu)));
    assert_eq!(pick(gib(24), None), Some(("gpt-oss-20b", Fit::Cpu)));
    assert_eq!(pick(15_500 * MIB, None), Some(("gemma-4-e4b", Fit::Cpu)));
    assert_eq!(pick(7_600 * MIB, None), Some(("granite-4.2-3b", Fit::Cpu)));
    // A small graphics card doesn't hide a better processor option.
    assert_eq!(
        pick(gib(32), Some(gib(2))),
        Some(("qwen3.6-35b-a3b", Fit::Cpu))
    );
    // Barely enough memory: the smallest model, with a shorter context.
    assert_eq!(pick(6 * GIB, None), Some(("granite-4.2-3b", Fit::Tight)));
    assert_eq!(pick(gib(4), None), None);
    assert_eq!(pick(0, None), None);
    let granite = entry("granite-4.2-3b").unwrap();
    assert_eq!(fit(granite, gib(4), None), Fit::No);
    assert_eq!(fit(granite, gib(4), Some(gib(6))), Fit::Gpu);
}

#[test]
fn installed_requires_the_exact_size() {
    let dir = tempfile::tempdir().unwrap();
    let model = entry("granite-4.2-3b").unwrap();
    assert!(installed(dir.path()).is_empty());
    let path = final_path(dir.path(), model.file);
    // Sparse files: the size is what counts, not the disk usage.
    fs::File::create(&path)
        .unwrap()
        .set_len(model.bytes - 1)
        .unwrap();
    assert!(
        installed(dir.path()).is_empty(),
        "a short file is not installed"
    );
    fs::File::create(&path)
        .unwrap()
        .set_len(model.bytes)
        .unwrap();
    let found = installed(dir.path());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0.id, model.id);
    assert_eq!(found[0].1, path);
    // A .part file is never a model.
    fs::File::create(part_path(dir.path(), "gemma-4-E4B_q4_0-it.gguf"))
        .unwrap()
        .set_len(entry("gemma-4-e4b").unwrap().bytes)
        .unwrap();
    assert_eq!(installed(dir.path()).len(), 1);
}

#[test]
fn a_full_disk_gets_a_plain_message() {
    let message = format!(
        "{:#}",
        plain_io(std::io::Error::from_raw_os_error(libc::ENOSPC), "write")
    );
    assert!(message.contains("The disk is full"), "{message}");
    assert!(message.contains("Resume"), "{message}");
    let other = format!(
        "{:#}",
        plain_io(
            std::io::Error::from_raw_os_error(libc::EACCES),
            "Cannot write"
        )
    );
    assert!(other.starts_with("Cannot write"), "{other}");
}

// ---------------------------------------------------------------------------
// Downloads against a local HTTP server
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Behaviour {
    /// Answer every request with 200 and the whole file.
    ignore_range: bool,
    /// Close the first response after this many body bytes.
    drop_first_after: Option<usize>,
    /// Answer with this status and no body.
    status: Option<u16>,
    /// Wait between 16 KiB chunks.
    chunk_delay: Option<Duration>,
    /// Claim the range starts at 0 while sending the requested part.
    wrong_range: bool,
}

struct Server {
    url: String,
    ranges: Arc<StdMutex<Vec<Option<String>>>>,
}

impl Server {
    fn requests(&self) -> Vec<Option<String>> {
        self.ranges.lock().unwrap().clone()
    }
}

async fn serve(body: Vec<u8>, behaviour: Behaviour) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/model.gguf", listener.local_addr().unwrap());
    let ranges: Arc<StdMutex<Vec<Option<String>>>> = Arc::default();
    let (body, log) = (Arc::new(body), ranges.clone());
    tokio::spawn(async move {
        let mut first = true;
        while let Ok((mut socket, _)) = listener.accept().await {
            let (body, behaviour, log) = (body.clone(), behaviour.clone(), log.clone());
            let drop_after = if first {
                behaviour.drop_first_after
            } else {
                None
            };
            first = false;
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buffer = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buffer[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&head).to_string();
                let range = text.lines().find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("range")
                        .then(|| value.trim().to_owned())
                });
                log.lock().unwrap().push(range.clone());
                if let Some(status) = behaviour.status {
                    let _ = socket
                        .write_all(
                            format!(
                                "HTTP/1.1 {status} Nope\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            )
                            .as_bytes(),
                        )
                        .await;
                    return;
                }
                let start = range
                    .as_deref()
                    .filter(|_| !behaviour.ignore_range)
                    .and_then(|r| r.strip_prefix("bytes=")?.strip_suffix('-')?.parse().ok());
                let len = body.len();
                let (status, from, extra) = match start {
                    Some(start) if start >= len => (
                        "416 Range Not Satisfiable",
                        len,
                        format!("Content-Range: bytes */{len}\r\n"),
                    ),
                    Some(start) => (
                        "206 Partial Content",
                        start,
                        format!(
                            "Content-Range: bytes {}-{}/{len}\r\n",
                            if behaviour.wrong_range { 0 } else { start },
                            len - 1
                        ),
                    ),
                    None => ("200 OK", 0, String::new()),
                };
                let slice = &body[from..];
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    slice.len()
                );
                if socket.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                let limit = drop_after.unwrap_or(slice.len()).min(slice.len());
                for chunk in slice[..limit].chunks(16 * 1024) {
                    if socket.write_all(chunk).await.is_err() {
                        return;
                    }
                    if let Some(delay) = behaviour.chunk_delay {
                        tokio::time::sleep(delay).await;
                    }
                }
                let _ = socket.shutdown().await;
            });
        }
    });
    Server { url, ranges }
}

fn body(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

fn spec_for(body: &[u8], url: &str, file: &str) -> Spec {
    Spec {
        id: file.into(),
        url: url.into(),
        file: file.into(),
        sha256: format!("{:x}", Sha256::digest(body)),
        bytes: body.len() as u64,
    }
}

fn plenty(_: &Path) -> std::io::Result<u64> {
    Ok(u64::MAX / 4)
}

fn nearly_full(_: &Path) -> std::io::Result<u64> {
    Ok(700 * MIB)
}

fn test_client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn wait_for_part(dir: &Path, file: &str, at_least: u64) {
    for _ in 0..6000 {
        if partial_len(dir, file) >= at_least {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the download never reached {at_least} bytes");
}

#[tokio::test]
async fn a_download_is_verified_before_it_gets_its_name() {
    let data = body(600 * 1024);
    let server = serve(data.clone(), Behaviour::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let downloads = Downloader::with_space_probe(plenty);
    let spec = spec_for(&data, &server.url, "fresh.gguf");
    assert_eq!(downloads.status(&spec, dir.path()).state, State::Available);
    downloads
        .start(spec.clone(), dir.path().to_path_buf(), test_client())
        .unwrap();
    downloads.idle().await;
    let status = downloads.status(&spec, dir.path());
    assert_eq!(status.state, State::Installed, "{status:?}");
    assert_eq!(status.done, spec.bytes);
    assert_eq!(fs::read(dir.path().join("fresh.gguf")).unwrap(), data);
    assert!(!part_path(dir.path(), "fresh.gguf").exists());
    assert_eq!(server.requests(), vec![None]);
    // Downloading again is refused; the file is already there.
    let again = downloads
        .start(spec, dir.path().to_path_buf(), test_client())
        .unwrap_err();
    assert!(format!("{again}").contains("already downloaded"));
}

#[tokio::test]
async fn pause_keeps_the_part_and_resume_asks_for_the_rest() {
    let data = body(2 * 1024 * 1024);
    let server = serve(
        data.clone(),
        Behaviour {
            chunk_delay: Some(Duration::from_millis(8)),
            ..Default::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let downloads = Downloader::with_space_probe(plenty);
    let spec = spec_for(&data, &server.url, "pause.gguf");
    downloads
        .start(spec.clone(), dir.path().to_path_buf(), test_client())
        .unwrap();
    wait_for_part(dir.path(), "pause.gguf", 256 * 1024).await;
    assert!(downloads.pause(dir.path(), "pause.gguf"));
    downloads.idle().await;
    let paused = downloads.status(&spec, dir.path());
    assert_eq!(paused.state, State::Paused, "{paused:?}");
    assert!(paused.done > 0 && paused.done < spec.bytes, "{paused:?}");
    assert!(!dir.path().join("pause.gguf").exists());
    let kept = partial_len(dir.path(), "pause.gguf");
    assert_eq!(kept, paused.done);

    // Resume: the part is re-read, then only the rest is requested.
    let server2 = serve(data.clone(), Behaviour::default()).await;
    let spec2 = Spec {
        url: server2.url.clone(),
        ..spec.clone()
    };
    downloads
        .start(spec2.clone(), dir.path().to_path_buf(), test_client())
        .unwrap();
    downloads.idle().await;
    assert_eq!(downloads.status(&spec2, dir.path()).state, State::Installed);
    assert_eq!(server2.requests(), vec![Some(format!("bytes={kept}-"))]);
    assert_eq!(fs::read(dir.path().join("pause.gguf")).unwrap(), data);
}

#[tokio::test]
async fn a_dropped_connection_keeps_the_part_for_resume() {
    let data = body(900 * 1024);
    let server = serve(
        data.clone(),
        Behaviour {
            drop_first_after: Some(300 * 1024),
            ..Default::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let downloads = Downloader::with_space_probe(plenty);
    let spec = spec_for(&data, &server.url, "drop.gguf");
    downloads
        .start(spec.clone(), dir.path().to_path_buf(), test_client())
        .unwrap();
    downloads.idle().await;
    let failed = downloads.status(&spec, dir.path());
    assert_eq!(failed.state, State::Failed, "{failed:?}");
    let error = failed.error.clone().unwrap();
    assert!(
        error.contains("Choose Resume to continue"),
        "plain, resumable message: {error}"
    );
    assert!(failed.done > 0 && failed.done <= 300 * 1024, "{failed:?}");
    downloads
        .start(spec.clone(), dir.path().to_path_buf(), test_client())
        .unwrap();
    downloads.idle().await;
    assert_eq!(downloads.status(&spec, dir.path()).state, State::Installed);
    assert_eq!(
        server.requests(),
        vec![None, Some(format!("bytes={}-", failed.done))]
    );
    assert_eq!(fs::read(dir.path().join("drop.gguf")).unwrap(), data);
}

#[tokio::test]
async fn a_server_that_ignores_the_range_starts_over() {
    let data = body(500 * 1024);
    let server = serve(
        data.clone(),
        Behaviour {
            ignore_range: true,
            ..Default::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    fs::write(part_path(dir.path(), "full.gguf"), &data[..100 * 1024]).unwrap();
    let downloads = Downloader::with_space_probe(plenty);
    let spec = spec_for(&data, &server.url, "full.gguf");
    assert_eq!(downloads.status(&spec, dir.path()).state, State::Paused);
    downloads
        .start(spec.clone(), dir.path().to_path_buf(), test_client())
        .unwrap();
    downloads.idle().await;
    assert_eq!(downloads.status(&spec, dir.path()).state, State::Installed);
    assert_eq!(server.requests(), vec![Some("bytes=102400-".into())]);
    assert_eq!(fs::read(dir.path().join("full.gguf")).unwrap(), data);
}

#[tokio::test]
async fn a_wrong_range_answer_restarts_once() {
    let data = body(400 * 1024);
    let server = serve(
        data.clone(),
        Behaviour {
            wrong_range: true,
            ..Default::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    fs::write(part_path(dir.path(), "wrong.gguf"), &data[..50 * 1024]).unwrap();
    let downloads = Downloader::with_space_probe(plenty);
    let spec = spec_for(&data, &server.url, "wrong.gguf");
    downloads
        .start(spec.clone(), dir.path().to_path_buf(), test_client())
        .unwrap();
    downloads.idle().await;
    assert_eq!(downloads.status(&spec, dir.path()).state, State::Installed);
    assert_eq!(server.requests(), vec![Some("bytes=51200-".into()), None]);
    assert_eq!(fs::read(dir.path().join("wrong.gguf")).unwrap(), data);
}

#[tokio::test]
async fn a_checksum_mismatch_deletes_the_file() {
    let data = body(300 * 1024);
    let server = serve(data.clone(), Behaviour::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let downloads = Downloader::with_space_probe(plenty);
    let spec = Spec {
        sha256: "0".repeat(64),
        ..spec_for(&data, &server.url, "bad.gguf")
    };
    downloads
        .start(spec.clone(), dir.path().to_path_buf(), test_client())
        .unwrap();
    downloads.idle().await;
    let failed = downloads.status(&spec, dir.path());
    assert_eq!(failed.state, State::Failed);
    assert!(failed.error.unwrap().contains("did not match its checksum"));
    assert_eq!(failed.done, 0, "nothing is left to resume");
    assert!(!dir.path().join("bad.gguf").exists());
    assert!(!part_path(dir.path(), "bad.gguf").exists());
}

#[tokio::test]
async fn a_damaged_part_is_caught_by_the_checksum_after_resume() {
    let data = body(300 * 1024);
    let server = serve(data.clone(), Behaviour::default()).await;
    let dir = tempfile::tempdir().unwrap();
    fs::write(part_path(dir.path(), "damaged.gguf"), vec![7u8; 64 * 1024]).unwrap();
    let downloads = Downloader::with_space_probe(plenty);
    let spec = spec_for(&data, &server.url, "damaged.gguf");
    downloads
        .start(spec.clone(), dir.path().to_path_buf(), test_client())
        .unwrap();
    downloads.idle().await;
    let failed = downloads.status(&spec, dir.path());
    assert_eq!(failed.state, State::Failed);
    assert!(failed.error.unwrap().contains("checksum"));
    assert!(!dir.path().join("damaged.gguf").exists());
    assert!(!part_path(dir.path(), "damaged.gguf").exists());
}

#[tokio::test]
async fn cancel_deletes_the_part() {
    let data = body(2 * 1024 * 1024);
    let server = serve(
        data.clone(),
        Behaviour {
            chunk_delay: Some(Duration::from_millis(8)),
            ..Default::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let downloads = Downloader::with_space_probe(plenty);
    let spec = spec_for(&data, &server.url, "cancel.gguf");
    downloads
        .start(spec.clone(), dir.path().to_path_buf(), test_client())
        .unwrap();
    wait_for_part(dir.path(), "cancel.gguf", 64 * 1024).await;
    assert!(downloads.busy());
    assert!(downloads.cancel(dir.path(), "cancel.gguf").unwrap());
    downloads.idle().await;
    assert!(!downloads.busy());
    assert_eq!(downloads.status(&spec, dir.path()).state, State::Available);
    assert!(!part_path(dir.path(), "cancel.gguf").exists());
    assert!(!dir.path().join("cancel.gguf").exists());
    // Cancel after a pause deletes the kept part too.
    fs::write(part_path(dir.path(), "cancel.gguf"), b"partial").unwrap();
    assert!(downloads.cancel(dir.path(), "cancel.gguf").unwrap());
    assert!(!part_path(dir.path(), "cancel.gguf").exists());
    assert!(!downloads.cancel(dir.path(), "cancel.gguf").unwrap());
}

#[tokio::test]
async fn not_enough_disk_space_is_refused_before_anything_downloads() {
    let data = body(1024);
    let server = serve(data.clone(), Behaviour::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let downloads = Downloader::with_space_probe(nearly_full);
    let spec = Spec {
        bytes: 2 * GB,
        ..spec_for(&data, &server.url, "big.gguf")
    };
    let error = format!(
        "{:#}",
        downloads
            .start(spec.clone(), dir.path().to_path_buf(), test_client())
            .unwrap_err()
    );
    assert!(error.starts_with("Not enough disk space"), "{error}");
    assert!(error.contains("Free up space"), "{error}");
    assert!(server.requests().is_empty(), "nothing was requested");
    assert!(!downloads.busy());
    assert_eq!(downloads.status(&spec, dir.path()).state, State::Available);
    // A kept part counts toward what is already there.
    let small = Spec {
        bytes: 900 * MIB,
        ..spec.clone()
    };
    fs::File::create(part_path(dir.path(), "big.gguf"))
        .unwrap()
        .set_len(800 * MIB)
        .unwrap();
    let error = downloads
        .start(small, dir.path().to_path_buf(), test_client())
        .map(|_| ())
        .err();
    assert!(error.is_none(), "100 MB more fits: {error:?}");
    downloads.cancel(dir.path(), "big.gguf").unwrap();
    downloads.idle().await;
}

#[tokio::test]
async fn a_refusing_guard_stops_the_download_and_keeps_the_part() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let data = body(4 * 1024 * 1024);
    let server = serve(
        data.clone(),
        Behaviour {
            chunk_delay: Some(Duration::from_millis(8)),
            ..Default::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let downloads = Downloader {
        guard_every: Duration::from_millis(20),
        ..Downloader::with_space_probe(plenty)
    };
    let online = Arc::new(AtomicBool::new(true));
    let flag = online.clone();
    let guard: Guard = Arc::new(move || {
        if flag.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err("Offline mode was turned on".into())
        }
    });
    let spec = spec_for(&data, &server.url, "guarded.gguf");
    downloads
        .start_guarded(
            spec.clone(),
            dir.path().to_path_buf(),
            test_client(),
            guard.clone(),
        )
        .unwrap();
    wait_for_part(dir.path(), "guarded.gguf", 128 * 1024).await;
    online.store(false, Ordering::SeqCst);
    downloads.idle().await;
    let stopped = downloads.status(&spec, dir.path());
    assert_eq!(stopped.state, State::Failed, "{stopped:?}");
    assert_eq!(stopped.error.as_deref(), Some("Offline mode was turned on"));
    assert!(stopped.done > 0 && stopped.done < spec.bytes, "{stopped:?}");
    // A refusing guard also stops a resume before any request.
    let before = server.requests().len();
    downloads
        .start_guarded(
            spec.clone(),
            dir.path().to_path_buf(),
            test_client(),
            guard.clone(),
        )
        .unwrap();
    downloads.idle().await;
    assert_eq!(server.requests().len(), before);
    assert_eq!(downloads.status(&spec, dir.path()).state, State::Failed);
    online.store(true, Ordering::SeqCst);
    downloads
        .start_guarded(spec.clone(), dir.path().to_path_buf(), test_client(), guard)
        .unwrap();
    downloads.idle().await;
    assert_eq!(downloads.status(&spec, dir.path()).state, State::Installed);
    assert_eq!(fs::read(dir.path().join("guarded.gguf")).unwrap(), data);
}

#[tokio::test]
async fn one_model_downloads_at_a_time() {
    let data = body(2 * 1024 * 1024);
    let server = serve(
        data.clone(),
        Behaviour {
            chunk_delay: Some(Duration::from_millis(8)),
            ..Default::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let downloads = Downloader::with_space_probe(plenty);
    downloads
        .start(
            spec_for(&data, &server.url, "a.gguf"),
            dir.path().to_path_buf(),
            test_client(),
        )
        .unwrap();
    let error = downloads
        .start(
            spec_for(&data, &server.url, "b.gguf"),
            dir.path().to_path_buf(),
            test_client(),
        )
        .unwrap_err();
    assert!(format!("{error}").contains("Another model is downloading"));
    downloads.cancel(dir.path(), "a.gguf").unwrap();
    downloads.idle().await;
}

#[tokio::test]
async fn http_errors_are_reported_and_keep_nothing() {
    let data = body(1024);
    let server = serve(
        data.clone(),
        Behaviour {
            status: Some(404),
            ..Default::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let downloads = Downloader::with_space_probe(plenty);
    let spec = spec_for(&data, &server.url, "missing.gguf");
    downloads
        .start(spec.clone(), dir.path().to_path_buf(), test_client())
        .unwrap();
    downloads.idle().await;
    let failed = downloads.status(&spec, dir.path());
    assert_eq!(failed.state, State::Failed);
    assert!(failed.error.unwrap().contains("HTTP 404"));
    assert_eq!(failed.done, 0);
    // Cancel clears the failure.
    downloads.cancel(dir.path(), "missing.gguf").unwrap();
    assert_eq!(downloads.status(&spec, dir.path()).state, State::Available);
}

#[tokio::test]
async fn an_unreachable_server_is_a_plain_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/model.gguf", listener.local_addr().unwrap());
    drop(listener);
    let dir = tempfile::tempdir().unwrap();
    let downloads = Downloader::with_space_probe(plenty);
    let spec = spec_for(b"x", &url, "offline.gguf");
    downloads
        .start(spec.clone(), dir.path().to_path_buf(), test_client())
        .unwrap();
    downloads.idle().await;
    let failed = downloads.status(&spec, dir.path());
    assert_eq!(failed.state, State::Failed);
    assert!(failed
        .error
        .unwrap()
        .starts_with("Could not reach the download server"));
}

#[test]
fn delete_removes_the_model_and_any_part() {
    let dir = tempfile::tempdir().unwrap();
    let downloads = Downloader::with_space_probe(plenty);
    let model = entry("granite-4.2-3b").unwrap();
    let spec = model.spec();
    fs::File::create(final_path(dir.path(), model.file))
        .unwrap()
        .set_len(model.bytes)
        .unwrap();
    fs::write(part_path(dir.path(), model.file), b"old part").unwrap();
    assert_eq!(downloads.status(&spec, dir.path()).state, State::Installed);
    assert!(downloads.delete(dir.path(), model.file).unwrap());
    assert!(!final_path(dir.path(), model.file).exists());
    assert!(!part_path(dir.path(), model.file).exists());
    assert_eq!(downloads.status(&spec, dir.path()).state, State::Available);
    assert!(!downloads.delete(dir.path(), model.file).unwrap());
}

#[test]
fn catalog_json_reports_fit_support_and_progress() {
    let dir = tempfile::tempdir().unwrap();
    let downloads = Downloader::with_space_probe(plenty);
    let granite = entry("granite-4.2-3b").unwrap();
    fs::File::create(final_path(dir.path(), granite.file))
        .unwrap()
        .set_len(granite.bytes)
        .unwrap();
    let gemma = entry("gemma-4-e4b").unwrap();
    fs::File::create(part_path(dir.path(), gemma.file))
        .unwrap()
        .set_len(GB)
        .unwrap();
    let archs: std::collections::HashSet<String> =
        ["granite", "gemma4", "qwen35moe"].map(String::from).into();
    let value = catalog_json(
        &downloads,
        dir.path(),
        16 * GIB,
        Some(("Test GPU", 8 * GIB)),
        Some(&archs),
        true,
    );
    assert_eq!(value["offline"], true);
    assert_eq!(value["recommended"], "gemma-4-e4b");
    assert_eq!(value["recommended_fit"], "gpu");
    assert_eq!(value["hardware"]["gpu"], "Test GPU");
    let row = |id: &str| {
        value["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .unwrap()
            .clone()
    };
    let installed = row("granite-4.2-3b");
    assert_eq!(installed["state"], "installed");
    assert_eq!(installed["fit"], "gpu");
    assert!(installed["model_id"]
        .as_str()
        .unwrap()
        .starts_with("local:gguf:"));
    let paused = row("gemma-4-e4b");
    assert_eq!(paused["state"], "paused");
    assert_eq!(paused["done"], GB);
    assert_eq!(paused["recommended"], true);
    assert!(paused["model_id"].is_null());
    let oss = row("gpt-oss-20b");
    assert_eq!(oss["supported"], false);
    assert!(oss["unsupported_reason"]
        .as_str()
        .unwrap()
        .contains("gpt-oss"));
    assert_eq!(row("qwen3.6-35b-a3b")["fit"], "no");
    // A model the runtime can't load is never the recommendation.
    let only_oss: std::collections::HashSet<String> = ["gpt-oss"].map(String::from).into();
    let value = catalog_json(
        &downloads,
        dir.path(),
        64 * GIB,
        Some(("GPU", 24 * GIB)),
        Some(&only_oss),
        false,
    );
    assert_eq!(value["recommended"], "gpt-oss-20b");
    assert_eq!(row("gemma-4-12b")["fit"], "cpu");
}

// ---------------------------------------------------------------------------
// Live checks (network; run on purpose)
// ---------------------------------------------------------------------------

/// Re-verify every pin against Hugging Face: the commit, size and SHA-256 the
/// resolve endpoint reports, and the architecture and memory estimate from
/// the file's own header (fetched with a range request).
/// `cargo test -p shadowcode-core --lib catalog_matches_hugging_face -- --ignored`
#[tokio::test]
#[ignore = "network: downloads each model's header from huggingface.co"]
async fn catalog_matches_hugging_face() {
    let head = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let client = client().unwrap();
    for model in CATALOG {
        let response = head.head(model.url()).send().await.unwrap();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .trim_matches('"')
                .to_owned()
        };
        assert_eq!(header("x-repo-commit"), model.commit, "{}", model.id);
        assert_eq!(
            header("x-linked-size"),
            model.bytes.to_string(),
            "{}",
            model.id
        );
        assert_eq!(header("x-linked-etag"), model.sha256, "{}", model.id);
        let prefix = client
            .get(model.url())
            .header(reqwest::header::RANGE, "bytes=0-33554431")
            .send()
            .await
            .unwrap();
        assert_eq!(prefix.status().as_u16(), 206, "{}", model.id);
        let bytes = prefix.bytes().await.unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), &bytes).unwrap();
        let parsed = crate::gguf::read_header(file.path()).unwrap();
        assert_eq!(
            parsed.architecture(),
            Some(model.architecture),
            "{}",
            model.id
        );
        assert!(parsed.template_mentions_tools(), "{} tools", model.id);
        let estimate = |ctx| crate::gguf::estimate_memory(&parsed, model.bytes, 0, ctx).total_bytes;
        assert_eq!(estimate(16_384), model.memory_bytes, "{}", model.id);
        assert_eq!(estimate(4_096), model.min_memory_bytes, "{}", model.id);
        println!("{} ok", model.id);
    }
}

/// Download one catalog model with ShadowCode's downloader, pausing and
/// resuming once on the way. Set SHADOWCODE_LIVE_DOWNLOAD_DIR to a scratch
/// folder (and optionally SHADOWCODE_LIVE_DOWNLOAD_ID; default: the
/// smallest model).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "network: downloads a model of several GB"]
async fn live_download_pause_resume_verify() {
    let dir = PathBuf::from(
        std::env::var("SHADOWCODE_LIVE_DOWNLOAD_DIR").expect("SHADOWCODE_LIVE_DOWNLOAD_DIR"),
    );
    let id = std::env::var("SHADOWCODE_LIVE_DOWNLOAD_ID").unwrap_or_else(|_| CATALOG[0].id.into());
    let model = entry(&id).expect("catalog id");
    let spec = model.spec();
    let downloads = Downloader::default();
    let started = Instant::now();
    downloads
        .start(spec.clone(), dir.clone(), client().unwrap())
        .unwrap();
    wait_for_part(&dir, model.file, 64 * MIB).await;
    assert!(downloads.pause(&dir, model.file));
    downloads.idle().await;
    let paused = downloads.status(&spec, &dir);
    assert_eq!(paused.state, State::Paused, "{paused:?}");
    println!("paused at {} bytes", paused.done);
    downloads
        .start(spec.clone(), dir.clone(), client().unwrap())
        .unwrap();
    loop {
        let status = downloads.status(&spec, &dir);
        match status.state {
            State::Installed => break,
            State::Failed => panic!("{status:?}"),
            _ => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
    println!(
        "{} installed and verified in {:?}",
        model.id,
        started.elapsed()
    );
    assert!(installed(&dir).iter().any(|(m, _)| m.id == model.id));
}
