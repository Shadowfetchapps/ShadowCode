//! Voice input: push-to-talk dictation into the composer.
//!
//! The engine records from the default microphone (`capture`), and on stop
//! turns the audio into text with one of two engines the user chooses in
//! Settings › Voice:
//! - `local` (default): whisper.cpp on the CPU with a model the user
//!   installed (`models`, `whisper`). Audio never leaves the machine.
//! - `openrouter`: only when picked explicitly and an OpenRouter key is set;
//!   the clip goes to an audio-capable model (`cloud`). Never a fallback.
//!
//! The text goes back to the window, which inserts it at the cursor; nothing
//! is sent to a model as a task.
pub mod audio;
pub mod capture;
pub mod cloud;
pub mod models;
pub mod text;
pub mod whisper;

use crate::{config::Config, paths::AppPaths};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub const ENGINES: &[&str] = &["local", "openrouter"];

/// Languages offered in Settings (whisper knows about 100; any two- or
/// three-letter code it supports also works in the config file).
pub const LANGUAGES: &[(&str, &str)] = &[
    ("auto", "Detect automatically"),
    ("en", "English"),
    ("ar", "Arabic"),
    ("zh", "Chinese"),
    ("cs", "Czech"),
    ("da", "Danish"),
    ("nl", "Dutch"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("de", "German"),
    ("el", "Greek"),
    ("he", "Hebrew"),
    ("hi", "Hindi"),
    ("hu", "Hungarian"),
    ("id", "Indonesian"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("no", "Norwegian"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("es", "Spanish"),
    ("sv", "Swedish"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
    ("vi", "Vietnamese"),
];

/// `voice:` in config.yaml.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct VoiceConfig {
    /// "local" (whisper.cpp) or "openrouter".
    pub engine: String,
    /// Local model id from the catalog.
    pub model: String,
    /// Language code, or "auto" (multilingual model / OpenRouter only).
    pub language: String,
    /// Audio-capable OpenRouter model for the `openrouter` engine.
    pub openrouter_model: String,
    /// Spoken "new line" / "new paragraph" become line breaks.
    pub voice_commands: bool,
    /// Show words while recording (local engine; costs some CPU).
    pub live_preview: bool,
    /// A recording stops taking audio after this long.
    pub max_seconds: u32,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            engine: "local".into(),
            model: "base.en".into(),
            language: "en".into(),
            openrouter_model: cloud::DEFAULT_MODEL.into(),
            voice_commands: true,
            live_preview: true,
            max_seconds: 120,
        }
    }
}

impl VoiceConfig {
    pub fn from_config(config: &Config) -> Result<Self> {
        match config.extra.get("voice") {
            None | Some(Value::Null) => Ok(Self::default()),
            Some(value) => {
                let parsed: Self = serde_json::from_value(value.clone())
                    .map_err(|e| anyhow::anyhow!("Invalid voice settings: {e}"))?;
                parsed.validate()?;
                Ok(parsed)
            }
        }
    }
    /// Like `from_config`, but an invalid section falls back to defaults.
    pub fn lenient(config: &Config) -> Self {
        Self::from_config(config).unwrap_or_default()
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            ENGINES.contains(&self.engine.as_str()),
            "voice.engine must be local or openrouter"
        );
        ensure!(
            models::entry(&self.model).is_some(),
            "voice.model must be one of: {}",
            models::CATALOG
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
                .join(", ")
        );
        ensure!(
            self.language == "auto"
                || ((2..=3).contains(&self.language.len())
                    && self.language.chars().all(|c| c.is_ascii_lowercase())),
            "voice.language must be auto or a language code such as en"
        );
        ensure!(
            !self.openrouter_model.is_empty()
                && self.openrouter_model.len() <= 200
                && self
                    .openrouter_model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./:~".contains(c)),
            "voice.openrouter_model must be an OpenRouter model id"
        );
        ensure!(
            (5..=600).contains(&self.max_seconds),
            "voice.max_seconds must be between 5 and 600"
        );
        Ok(())
    }
}

pub fn data_dir(paths: &AppPaths) -> PathBuf {
    paths.data.join("voice")
}

/// The local model dictation uses: the chosen one when installed, otherwise
/// the first installed one (so installing any model is enough to start).
pub fn active_model(data_dir: &Path, voice: &VoiceConfig) -> Option<&'static models::VoiceModel> {
    models::entry(&voice.model)
        .filter(|m| models::installed(data_dir, m))
        .or_else(|| {
            models::CATALOG
                .iter()
                .find(|m| models::installed(data_dir, m))
        })
}

/// Where a transcription will run, checked before recording starts so the
/// user hears about a missing model or key without speaking first.
pub enum Engine {
    Local {
        model: PathBuf,
        language: String,
    },
    OpenRouter {
        key: String,
        model: String,
        language: String,
    },
}

impl Engine {
    pub fn name(&self) -> &'static str {
        match self {
            Engine::Local { .. } => "local",
            Engine::OpenRouter { .. } => "openrouter",
        }
    }
}

pub fn engine(paths: &AppPaths, config: &Config, voice: &VoiceConfig) -> Result<Engine> {
    match voice.engine.as_str() {
        "openrouter" => {
            ensure!(
                !config.offline(),
                "ShadowCode is in offline mode. Switch to online, or choose Local as the voice engine in Settings › Voice."
            );
            let key = crate::openrouter::key(paths).context(
                "Voice is set to OpenRouter, but no OpenRouter API key is saved. Add one in Settings › Accounts, or choose Local.",
            )?;
            Ok(Engine::OpenRouter {
                key,
                model: voice.openrouter_model.clone(),
                language: voice.language.clone(),
            })
        }
        _ => {
            ensure!(whisper::cpu_supported(), whisper::CPU_MESSAGE);
            let dir = data_dir(paths);
            let model = match active_model(&dir, voice) {
                Some(model) => model,
                None => {
                    let wanted = models::entry(&voice.model).context("Unknown voice model")?;
                    bail!(
                        "No voice model is installed. Open Settings › Voice and install {} ({} MB).",
                        wanted.name,
                        wanted.bytes / (1 << 20)
                    )
                }
            };
            Ok(Engine::Local {
                model: models::model_path(&dir, model),
                language: if model.english_only {
                    "en".into()
                } else {
                    voice.language.clone()
                },
            })
        }
    }
}

/// Transcribe mono samples with `engine` and clean the result.
pub async fn transcribe(
    engine: Engine,
    mono: Vec<f32>,
    rate: u32,
    commands: bool,
) -> Result<Value> {
    let seconds = if rate == 0 {
        0.0
    } else {
        mono.len() as f32 / rate as f32
    };
    let name = engine.name();
    if seconds < 0.3 {
        return Ok(
            json!({"text": "", "engine": name, "seconds": seconds, "ms": 0,
            "message": "Nothing was recorded. Hold the button a little longer."}),
        );
    }
    if !audio::has_speech(&mono, rate) {
        return Ok(
            json!({"text": "", "engine": name, "seconds": seconds, "ms": 0,
            "message": "Nothing was heard. Check the microphone in your sound settings."}),
        );
    }
    let started = Instant::now();
    let raw = match engine {
        Engine::Local { model, language } => tokio::task::spawn_blocking(move || {
            let samples = audio::for_whisper(&mono, rate);
            whisper::transcribe(&model, &samples, &language, None)
        })
        .await
        .context("The voice engine stopped")??,
        Engine::OpenRouter {
            key,
            model,
            language,
        } => {
            cloud::transcribe(
                &crate::openrouter::base_url(),
                &key,
                &model,
                &mono,
                rate,
                &language,
            )
            .await?
        }
    };
    let text = text::finish(&raw, commands);
    Ok(json!({
        "text": text,
        "engine": name,
        "seconds": seconds,
        "ms": started.elapsed().as_millis() as u64,
        "message": if text.is_empty() { Some("No speech was recognised.") } else { None },
    }))
}

// ---------------------------------------------------------------------------
// The one recording in progress
// ---------------------------------------------------------------------------

struct Preview {
    text: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

struct Active {
    recorder: capture::Recorder,
    engine: Engine,
    voice: VoiceConfig,
    preview: Option<Preview>,
}

static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);

fn active() -> Result<std::sync::MutexGuard<'static, Option<Active>>> {
    ACTIVE
        .lock()
        .map_err(|_| anyhow::anyhow!("Voice input stopped; restart ShadowCode"))
}

/// Start recording. Blocking (opening the device takes a moment).
pub fn start(paths: &AppPaths, config: &Config) -> Result<Value> {
    let voice = VoiceConfig::lenient(config);
    let engine = engine(paths, config, &voice)?;
    let mut slot = active()?;
    ensure!(slot.is_none(), "Already listening");
    let recorder = capture::Recorder::start(voice.max_seconds)?;
    let preview = match (&engine, voice.live_preview) {
        // Without a preview thread (the system is out of threads) recording
        // still works; the words just appear when it stops.
        (Engine::Local { model, language }, true) => start_preview(
            recorder.shared.clone(),
            model.clone(),
            language.clone(),
            voice.voice_commands,
        ),
        _ => None,
    };
    let device = recorder.device.clone();
    let name = engine.name();
    *slot = Some(Active {
        recorder,
        engine,
        voice,
        preview,
    });
    Ok(json!({"ok": true, "engine": name, "device": device}))
}

/// Level and live text for the listening indicator. Cheap; polled.
pub fn recording() -> Value {
    let Ok(slot) = ACTIVE.lock() else {
        return json!({"active": false});
    };
    match slot.as_ref() {
        None => json!({"active": false}),
        Some(active) => {
            let shared = &active.recorder.shared;
            json!({
                "active": true,
                "engine": active.engine.name(),
                "device": active.recorder.device,
                "level": shared.level(),
                "seconds": shared.seconds(),
                "max_seconds": active.voice.max_seconds,
                "full": shared.full(),
                "error": shared.error(),
                "partial": active.preview.as_ref().and_then(|p| p.text.lock().ok().map(|t| t.clone())).unwrap_or_default(),
            })
        }
    }
}

fn take() -> Result<Option<Active>> {
    Ok(active()?.take())
}

fn stop_preview(preview: Option<Preview>) {
    if let Some(preview) = preview {
        preview.stop.store(true, Ordering::Relaxed);
        let _ = preview.thread.join();
    }
}

/// Stop recording and transcribe.
pub async fn stop() -> Result<Value> {
    let Some(active) = take()? else {
        bail!("Not listening");
    };
    let Active {
        recorder,
        engine,
        voice,
        preview,
    } = active;
    let (mono, rate) = tokio::task::spawn_blocking(move || {
        stop_preview(preview);
        recorder.finish()
    })
    .await
    .context("The recording thread stopped")?;
    transcribe(engine, mono, rate, voice.voice_commands).await
}

/// Stop recording and throw the audio away.
pub async fn cancel() -> Result<bool> {
    let Some(active) = take()? else {
        return Ok(false);
    };
    tokio::task::spawn_blocking(move || {
        stop_preview(active.preview);
        drop(active.recorder);
    })
    .await
    .context("The recording thread stopped")?;
    Ok(true)
}

pub fn is_recording() -> bool {
    ACTIVE.lock().is_ok_and(|slot| slot.is_some())
}

/// Re-transcribe the growing recording about once a second so the window can
/// show words while the user talks. Each run is cut short when the recording
/// stops; the final transcription covers the whole clip.
fn start_preview(
    shared: Arc<capture::Shared>,
    model: PathBuf,
    language: String,
    commands: bool,
) -> Option<Preview> {
    let text = Arc::new(Mutex::new(String::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (out, flag) = (text.clone(), stop.clone());
    let thread = std::thread::Builder::new()
        .name("shadowcode-voice-preview".into())
        .spawn(move || preview_loop(&shared, &model, &language, commands, &out, &flag))
        .map_err(|error| tracing::warn!("Live voice preview unavailable: {error}"))
        .ok()?;
    Some(Preview { text, stop, thread })
}

fn preview_loop(
    shared: &capture::Shared,
    model: &Path,
    language: &str,
    commands: bool,
    out: &Mutex<String>,
    stop: &Arc<AtomicBool>,
) {
    let mut last_total = 0f32;
    while !stop.load(Ordering::Relaxed) {
        for _ in 0..10 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let rate = shared.rate();
        // The last 25 seconds are enough to show progress and stay quick.
        let total = shared.seconds();
        if rate == 0 || total < 1.0 || total == last_total {
            continue;
        }
        last_total = total;
        let tail = shared.tail(25.0);
        if !audio::has_speech(&tail, rate) {
            continue;
        }
        let samples = audio::for_whisper(&tail, rate);
        match whisper::transcribe(model, &samples, language, Some(stop.clone())) {
            Ok(raw) => {
                if let Ok(mut slot) = out.lock() {
                    *slot = text::finish(&raw, commands);
                }
            }
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_validation() {
        let mut config = Config::default();
        assert_eq!(
            VoiceConfig::from_config(&config).unwrap(),
            VoiceConfig::default()
        );
        let defaults = VoiceConfig::default();
        assert_eq!(defaults.engine, "local", "cloud is never the default");
        defaults.validate().unwrap();
        for (field, value) in [
            ("engine", json!("whisper-api")),
            ("model", json!("large-v3")),
            ("language", json!("English")),
            ("openrouter_model", json!("bad model id")),
            ("max_seconds", json!(1)),
        ] {
            let mut section = serde_json::to_value(&defaults).unwrap();
            section[field] = value;
            config.extra.insert("voice".into(), section);
            assert!(
                VoiceConfig::from_config(&config).is_err(),
                "{field} accepted"
            );
            assert_eq!(VoiceConfig::lenient(&config), defaults);
        }
        config.extra.insert(
            "voice".into(),
            json!({"engine": "openrouter", "language": "auto"}),
        );
        let parsed = VoiceConfig::from_config(&config).unwrap();
        assert_eq!(parsed.engine, "openrouter");
        assert_eq!(parsed.model, "base.en");
    }

    #[test]
    fn engine_explains_missing_model_key_and_offline() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::isolated(root.path()).unwrap();
        let mut config = Config::default();
        if whisper::cpu_supported() {
            let error = engine(&paths, &config, &VoiceConfig::default())
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains("No voice model is installed"), "{error}");
            assert!(error.contains("Settings › Voice"), "{error}");
            // An installed multilingual model keeps the chosen language; an
            // English-only one always reads English.
            let dir = data_dir(&paths);
            for id in ["base", "tiny.en"] {
                let model = models::entry(id).unwrap();
                let path = models::model_path(&dir, model);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::File::create(&path)
                    .unwrap()
                    .set_len(model.bytes)
                    .unwrap();
                let voice = VoiceConfig {
                    model: id.into(),
                    language: "de".into(),
                    ..Default::default()
                };
                match engine(&paths, &config, &voice).unwrap() {
                    Engine::Local { model: p, language } => {
                        assert_eq!(p, path);
                        assert_eq!(language, if id == "base" { "de" } else { "en" });
                    }
                    _ => panic!("expected local"),
                }
            }
            // base.en is chosen by default but not installed: the first
            // installed model is used instead.
            let fallback = active_model(&dir, &VoiceConfig::default()).unwrap();
            assert_eq!(fallback.id, "tiny.en");
        }
        let cloud = VoiceConfig {
            engine: "openrouter".into(),
            ..Default::default()
        };
        if crate::openrouter::key(&paths).is_none() {
            let error = engine(&paths, &config, &cloud).err().unwrap().to_string();
            assert!(error.contains("no OpenRouter API key"), "{error}");
        }
        config.network.mode = crate::config::NetworkMode::Offline;
        let error = engine(&paths, &config, &cloud).err().unwrap().to_string();
        assert!(error.contains("offline"), "{error}");
    }

    #[tokio::test]
    async fn short_clips_are_not_transcribed() {
        let result = transcribe(
            Engine::Local {
                model: PathBuf::from("/nonexistent"),
                language: "en".into(),
            },
            vec![0.0; 1_000],
            16_000,
            true,
        )
        .await
        .unwrap();
        assert_eq!(result["text"], "");
        assert!(result["message"]
            .as_str()
            .unwrap()
            .contains("Nothing was recorded"));
    }

    #[tokio::test]
    async fn stop_and_cancel_without_a_recording() {
        if is_recording() {
            return;
        }
        assert!(stop()
            .await
            .unwrap_err()
            .to_string()
            .contains("Not listening"));
        assert!(!cancel().await.unwrap());
        assert_eq!(recording()["active"], false);
    }
}
