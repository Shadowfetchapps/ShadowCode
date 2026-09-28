//! Local transcription with whisper.cpp (through whisper-rs), CPU only.
//!
//! One model stays loaded after use so the next dictation starts at once;
//! it is dropped after [`IDLE_UNLOAD`] without dictation, and removing the
//! model or choosing another one drops it at once. Runs are serialised by the
//! same lock, and a run can be cut short through its abort flag (the live
//! preview uses this when the user stops talking).
use anyhow::{ensure, Context, Result};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard, Once,
    },
    time::{Duration, Instant},
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

struct Loaded {
    path: PathBuf,
    context: WhisperContext,
}

/// A loaded model is freed after this long without a dictation.
pub const IDLE_UNLOAD: Duration = Duration::from_secs(5 * 60);

static LOADED: IdleSlot<Loaded> = IdleSlot::new(IDLE_UNLOAD);
static QUIET: Once = Once::new();

/// Holds one value while it is in use and drops it after `idle` without use.
/// A background thread runs only while a value is held.
struct IdleSlot<T: Send + 'static> {
    held: Mutex<Held<T>>,
    idle: Duration,
    /// A reaper thread is waiting to drop the value (changed under `held`).
    reaper: AtomicBool,
}
struct Held<T> {
    value: Option<T>,
    used: Option<Instant>,
}
/// The locked slot. Dropping it marks the value as just used.
struct IdleGuard<T: Send + 'static> {
    slot: &'static IdleSlot<T>,
    guard: MutexGuard<'static, Held<T>>,
}
impl<T: Send + 'static> IdleSlot<T> {
    const fn new(idle: Duration) -> Self {
        Self {
            held: Mutex::new(Held {
                value: None,
                used: None,
            }),
            idle,
            reaper: AtomicBool::new(false),
        }
    }
    fn lock(&'static self) -> Result<IdleGuard<T>> {
        let guard = self
            .held
            .lock()
            .map_err(|_| anyhow::anyhow!("The voice engine stopped; restart ShadowCode"))?;
        Ok(IdleGuard { slot: self, guard })
    }
    /// Wait until the value has been unused for `idle`, then drop it.
    fn reap(&'static self) {
        loop {
            let wait = {
                let Ok(mut held) = self.held.lock() else {
                    return;
                };
                let since = held.used.map_or(self.idle, |used| used.elapsed());
                if held.value.is_none() || since >= self.idle {
                    let value = held.value.take();
                    self.reaper.store(false, Ordering::Release);
                    drop(held);
                    drop(value);
                    return;
                }
                self.idle - since
            };
            std::thread::sleep(wait.max(Duration::from_millis(10)));
        }
    }
}
impl<T: Send + 'static> std::ops::Deref for IdleGuard<T> {
    type Target = Option<T>;
    fn deref(&self) -> &Option<T> {
        &self.guard.value
    }
}
impl<T: Send + 'static> std::ops::DerefMut for IdleGuard<T> {
    fn deref_mut(&mut self) -> &mut Option<T> {
        &mut self.guard.value
    }
}
impl<T: Send + 'static> Drop for IdleGuard<T> {
    fn drop(&mut self) {
        self.guard.used = Some(Instant::now());
        if self.guard.value.is_some() && !self.slot.reaper.swap(true, Ordering::AcqRel) {
            let slot = self.slot;
            let spawned = std::thread::Builder::new()
                .name("shadowcode-voice-unload".into())
                .spawn(move || slot.reap());
            if spawned.is_err() {
                // The value then stays loaded, as before idle unloading.
                self.slot.reaper.store(false, Ordering::Release);
            }
        }
    }
}

/// whisper.cpp is built for x86-64 with AVX2, FMA and F16C (see
/// `.cargo/config.toml`); older CPUs would crash inside it, so they get a
/// clear message instead.
pub fn cpu_supported() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("fma")
            && std::arch::is_x86_feature_detected!("f16c")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        true
    }
}

pub const CPU_MESSAGE: &str = "Local dictation needs a processor with AVX2 (most made since 2013). Choose OpenRouter as the voice engine instead.";

/// The whisper.cpp revision compiled in.
pub fn version() -> &'static str {
    whisper_rs::WHISPER_CPP_VERSION
}

/// Drop the loaded model if it is `path`.
pub fn unload(path: &Path) {
    if let Ok(mut loaded) = LOADED.lock() {
        if loaded.as_ref().is_some_and(|l| l.path == path) {
            *loaded = None;
        }
    }
}

/// Whether a local voice model is loaded now.
pub fn loaded() -> bool {
    LOADED.held.lock().is_ok_and(|held| held.value.is_some())
}

/// Segments more likely than this to be silence are dropped.
const NO_SPEECH: f32 = 0.6;

pub fn threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8) as i32
}

/// Transcribe 16 kHz mono samples. `language` is a code such as "en", or
/// "auto" to detect. Blocking: call from a worker thread.
pub fn transcribe(
    model: &Path,
    samples: &[f32],
    language: &str,
    abort: Option<Arc<AtomicBool>>,
) -> Result<String> {
    ensure!(cpu_supported(), CPU_MESSAGE);
    QUIET.call_once(whisper_rs::install_logging_hooks);
    let mut loaded = LOADED.lock()?;
    if loaded.as_ref().is_none_or(|l| l.path != model) {
        *loaded = None;
        let path = model
            .to_str()
            .context("The model path is not valid UTF-8")?;
        let mut params = WhisperContextParameters::default();
        params.use_gpu(false);
        let context = WhisperContext::new_with_params(path, params)
            .map_err(|e| anyhow::anyhow!("Could not load the voice model: {e}"))?;
        *loaded = Some(Loaded {
            path: model.to_owned(),
            context,
        });
    }
    let context = &loaded.as_ref().context("No voice model is loaded")?.context;
    let mut state = context
        .create_state()
        .map_err(|e| anyhow::anyhow!("Could not start the voice model: {e}"))?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    let language = if context.is_multilingual() {
        language
    } else {
        "en"
    };
    params.set_language(Some(language));
    params.set_n_threads(threads());
    params.set_translate(false);
    params.set_no_context(true);
    params.set_no_timestamps(true);
    params.set_suppress_blank(true);
    params.set_no_speech_thold(NO_SPEECH);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    if let Some(flag) = abort.clone() {
        params.set_abort_callback_safe(move || flag.load(Ordering::Relaxed));
    }
    let result = state.full(params, samples);
    if abort.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        anyhow::bail!("Stopped");
    }
    result.map_err(|e| anyhow::anyhow!("Transcription failed: {e}"))?;
    let mut text = String::new();
    for segment in state.as_iter() {
        // Whisper invents short words ("you", "Thank you.") for silence and
        // noise; it also says how likely a segment is to be no speech.
        if segment.no_speech_probability() > NO_SPEECH {
            continue;
        }
        if let Ok(part) = segment.to_str_lossy() {
            text.push_str(&part);
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the real model when `SHADOWCODE_WHISPER_TEST_MODEL` points at a
    /// ggml tiny/base model (kept in a scratch directory, never in the repo),
    /// optionally with `SHADOWCODE_WHISPER_TEST_WAV` for a speech clip:
    /// `cargo test -p shadowcode-core --offline voice::whisper -- --ignored`
    #[test]
    #[ignore = "needs a whisper model file"]
    fn transcribes_with_a_local_model() {
        let model = PathBuf::from(
            std::env::var("SHADOWCODE_WHISPER_TEST_MODEL")
                .expect("set SHADOWCODE_WHISPER_TEST_MODEL"),
        );
        // Silence never reaches whisper (see `audio::has_speech`); this only
        // checks that a run on it completes.
        let silence = vec![0.0f32; 16_000 * 2];
        let text = transcribe(&model, &silence, "en", None).unwrap();
        eprintln!("silence: {text:?}");
        if let Ok(wav) = std::env::var("SHADOWCODE_WHISPER_TEST_WAV") {
            let (mono, rate) =
                super::super::audio::decode_wav(&std::fs::read(wav).unwrap()).unwrap();
            let samples = super::super::audio::for_whisper(&mono, rate);
            let started = std::time::Instant::now();
            let text = transcribe(&model, &samples, "en", None).unwrap();
            eprintln!("{:?} in {:?}", text, started.elapsed());
            assert!(super::super::audio::has_speech(&mono, rate));
            assert!(!super::super::text::clean(&text).is_empty());
        }
        // An abort flag that is already set stops the run.
        let stop = Arc::new(AtomicBool::new(true));
        assert!(transcribe(&model, &silence, "en", Some(stop)).is_err());
        unload(&model);
        assert!(!loaded());
    }

    /// The loaded value is dropped only after `idle` without use; using it
    /// again starts the wait over, and a later load is unloaded again.
    #[test]
    fn idle_values_are_dropped_after_the_idle_time() {
        use std::sync::atomic::AtomicUsize;
        static DROPPED: AtomicUsize = AtomicUsize::new(0);
        struct Model;
        impl Drop for Model {
            fn drop(&mut self) {
                DROPPED.fetch_add(1, Ordering::SeqCst);
            }
        }
        const IDLE: Duration = Duration::from_millis(1000);
        static SLOT: IdleSlot<Model> = IdleSlot::new(IDLE);
        let held = || SLOT.held.lock().unwrap().value.is_some();
        let wait_for_drop = |count: usize| {
            let started = Instant::now();
            while DROPPED.load(Ordering::SeqCst) < count {
                assert!(
                    started.elapsed() < Duration::from_secs(20),
                    "the idle value was never dropped"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        *SLOT.lock().unwrap() = Some(Model);
        let loaded_at = Instant::now();
        std::thread::sleep(Duration::from_millis(400));
        // Used again before the idle time ran out (unless this machine
        // stalled for longer than that).
        let still = SLOT.lock().unwrap().is_some();
        let used = Instant::now();
        if loaded_at.elapsed() < IDLE {
            assert!(still);
        }
        std::thread::sleep(Duration::from_millis(700));
        let still = held();
        if used.elapsed() < IDLE {
            assert!(still, "dropped although it was used less than IDLE ago");
            assert_eq!(DROPPED.load(Ordering::SeqCst), 0);
        }
        wait_for_drop(1);
        assert!(used.elapsed() + Duration::from_millis(50) >= IDLE);
        assert!(!held());
        // A new load gets its own unload.
        *SLOT.lock().unwrap() = Some(Model);
        assert!(held());
        wait_for_drop(2);
        assert!(!held());
        assert!(!SLOT.reaper.load(Ordering::SeqCst));
    }
}
