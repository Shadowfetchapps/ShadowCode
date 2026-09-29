# Voice input

Dictate into the message box instead of typing. The words are inserted where
the cursor is; nothing is sent until you press Send.

## Use it

- **Hold** the microphone button next to Attach (or **Ctrl+Shift+Space**)
  while you talk, and let go to insert the text.
- Or **click** it once (tap the shortcut) to start, and again to stop.
- **Esc** or the **×** next to the button throws the recording away.
- While you talk, a small pill above the button shows the time, a level meter
  in the button shows the microphone is hearing you, and (with a local model)
  the words recognised so far.
- **Undo** next to the button removes the text that was just inserted, as long
  as you have not edited it since.
- Say **"new line"** or **"new paragraph"** to break lines (English; turn this
  off in Settings if you dictate those words).

A recording stops taking audio after two minutes (change it in Settings); the
button then stops by itself and inserts what it heard.

ShadowCode records from your system's default microphone. Choose which one
that is in your desktop's sound settings.

## Set it up: Settings › Voice

Nothing is downloaded until you ask. The first time you press the mic without
a model, Settings › Voice opens and says what to install.

| Model | Size | Notes |
| --- | --- | --- |
| Whisper base (English) | 141 MB | Recommended; about a second per sentence on a laptop CPU |
| Whisper tiny (English) | 74 MB | Fastest, more mistakes |
| Whisper base (multilingual) | 141 MB | About 100 languages; choose one or "Detect automatically" |

Models come from the `ggerganov/whisper.cpp` repository on Hugging Face,
pinned to commit `5359861c739e955e79d9a303bcbc70fb988958b1`; each file must
match its recorded size and SHA-256 before it is used, and a download that
receives nothing for 60 seconds stops with an error you can retry (the same checked
download as the embedding models). They are MIT licensed and are stored in
`~/.local/share/shadow-agent/voice/models/` (the profile's data directory).
**Remove** deletes the file. If the chosen model is not installed, any
installed one is used. Installs are refused in offline mode. A model stays
in memory after a dictation, so the next one starts at once, and is freed
after five minutes without dictation.

Other settings: language (fixed to English for the English-only models),
voice commands, live preview (re-reads the recording about once a second
while you talk; costs some CPU), and the longest recording.

## Where the audio goes

**On this computer (default).** Audio is captured and transcribed inside the
ShadowCode process with [whisper.cpp](https://github.com/ggml-org/whisper.cpp)
on the CPU. It never leaves the machine and is never written to disk; it is
discarded after the text is returned.

**OpenRouter (optional).** Only if you pick **OpenRouter** under *Transcribe
with* and have an OpenRouter API key in Settings › Accounts. Each recording is
then sent (16 kHz mono WAV, base64 `input_audio` part) to an audio-capable
chat model on `https://openrouter.ai/api/v1/chat/completions` and billed to
your key. The default model is `google/gemini-3.1-flash-lite`: it counts
32 audio tokens per second, so a minute of speech is about 1,900 input tokens,
roughly **$0.001 per minute** at September 2026 prices ($0.50 per million
audio tokens, plus a few hundred text tokens). You can enter another
audio-capable OpenRouter model id. It is off by default, is never used as a
fallback when the local engine is not set up, and is refused in offline mode.

## Why the engine records, not the window

The desktop window is WebKitGTK. Its `getUserMedia` is disabled by default
(`enable-media-stream`), Tauri's Linux webview does not answer WebKitGTK's
permission requests (so they are denied), and capture would also need
GStreamer's PipeWire/PulseAudio plugins inside the AppImage. Recording in the
Rust engine with [cpal](https://github.com/RustAudio/cpal) over ALSA avoids
all three: on PipeWire and PulseAudio desktops the default ALSA device routes
to the desktop's default microphone. The only new system library is
`libasound.so.2`, which every Linux desktop has (the Debian package depends on
`libasound2`).

Audio is kept at the device's rate and channel count, averaged to mono, and
resampled to 16 kHz with a windowed-sinc filter before whisper sees it.
Recordings that are silent or only faint noise (nothing louder than about
-45 dBFS for 100 ms) are not transcribed, because whisper tends to invent
words such as "you" for silence.

## Requirements and limits

- x86-64 processor with AVX2, FMA and F16C (most made since 2013). The
  built-in whisper.cpp is compiled for that baseline (see `.cargo/config.toml`),
  not for the build machine. Older processors get a clear message and can use
  OpenRouter.
- CPU only. GPU (Vulkan) builds of whisper.cpp are possible but not shipped.
- Voice commands are English words.
- Over remote access (a phone or another browser), the microphone button is
  hidden: ShadowCode never switches on the host computer's microphone for a
  remote device.

## Package size

whisper.cpp (with ggml's CPU backend) is linked statically into the
`shadowcode` binary, with cpal and the ALSA bindings. Measured on the
release build: the stripped binary grows by 1.7 MB (44.2 MB → 45.9 MB,
+3.9%), about 0.55 MB once compressed, so the AppImage grows by well under
1%. The binary now also links `libstdc++.so.6` (already required by the
bundled WebKitGTK and taken from the host like before) and `libasound.so.2`
(from the host). Models are never bundled.

## API

`/api/voice/*` routes are listed in the
[API contract](API_CONTRACT.md#voice).
