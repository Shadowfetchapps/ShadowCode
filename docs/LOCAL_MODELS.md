# Local models

ShadowCode runs GGUF models on this computer with a pinned build of
[llama.cpp](https://github.com/ggml-org/llama.cpp) that ships with the app. It
needs no Ollama, LM Studio or other daemon, and no account. It downloads a
model only when you choose one from its [list of free
models](#download-a-free-model), and deletes only files it downloaded, when you
choose **Delete**. Local rows appear under **On this computer** in the picker
and use ShadowCode's own agent loop, tools and permissions.

## The runtime

- **Location.** The AppImage and the deb carry the runtime in
  `usr/lib/shadowcode`: `llama-server`, its libraries (`$ORIGIN` links),
  loadable CPU variants for every x86-64 level, a Vulkan module, `COMMIT`,
  `architectures.txt` and `NOTICES/`. `scripts/install-appimage.sh` installs
  the AppImage's copy into `~/.local/lib/shadowcode`.
- **Which copy is used.** ShadowCode uses the first of these that exists:
  1. `local_engine.llama_binary`, if set.
  2. The runtime bundled with the running executable, if it is newer than the
     managed copy.
  3. `~/.local/lib/shadowcode`.
  4. The bundle.
  5. `SHADOWCODE_LLAMA_SERVER`.

  It never uses `llama-cli` or a `llama-server` found on `PATH`.
- **Readiness.** The runtime is *Ready* only after `llama-server --version`
  succeeds. **Settings › Local models › Runtime** shows its path, origin
  (bundled, managed or other), version, commit and backend.
- **Hardware.** Devices come from `llama-server --list-devices`, and RAM from
  `/proc/meminfo`. The Vulkan module loads only when a Vulkan loader and driver
  are present (`libvulkan1` plus your GPU driver). Otherwise the runtime stays
  on the CPU.

## Download a free model

**Settings › Local models › Download a free model** lists a few GGUF models
chosen for coding with tools, all under the Apache-2.0 license. The first run
offers the recommended one when no other model is ready (see the [user
guide](USER_GUIDE.md#open-a-project)).

| Model | Download | Memory (16K / 4K context) | Good for | Source, pinned commit |
| --- | --- | --- | --- | --- |
| Granite 4.2 3B, Q4_K_M (IBM) | 2.1 GB | 4.1 / 3.2 GB | Any computer; fine on a processor | [`ibm-granite/granite-4.2-3b-GGUF`](https://huggingface.co/ibm-granite/granite-4.2-3b-GGUF/tree/c40945d71cd90f249a56985e8155551a9188dc30) `c40945d` |
| Gemma 4 E4B, Q4_0 QAT (Google) | 4.8 GB | 6.1 / 5.7 GB | Laptops with 16 GB; fine on a processor | [`google/gemma-4-E4B-it-qat-q4_0-gguf`](https://huggingface.co/google/gemma-4-E4B-it-qat-q4_0-gguf/tree/4b4a2c1d584be7264f87aac328a1bc739ce81b6c) `4b4a2c1` |
| Gemma 4 12B, Q4_0 QAT (Google) | 6.5 GB | 8.0 / 7.8 GB | A graphics card with 10 GB or more | [`google/gemma-4-12B-it-qat-q4_0-gguf`](https://huggingface.co/google/gemma-4-12B-it-qat-q4_0-gguf/tree/29d097773436b69ff9feafd636ab4cf873786537) `29d0977` |
| gpt-oss 20B, MXFP4 (OpenAI) | 11.3 GB | 12.8 / 12.2 GB | A 16 GB graphics card, or 24 GB of memory | [`ggml-org/gpt-oss-20b-GGUF`](https://huggingface.co/ggml-org/gpt-oss-20b-GGUF/tree/ef9b12f2ff56c69cf32153a02784e7a3c88bf524) `ef9b12f` |
| Qwen3.6 35B-A3B, Q4_K_M (Qwen) | 19 GB | 21 / 20.1 GB | A 24 GB graphics card, or 32 GB of memory | [`ggml-org/Qwen3.6-35B-A3B-GGUF`](https://huggingface.co/ggml-org/Qwen3.6-35B-A3B-GGUF/tree/baec3ebee244827cda0f4557eafa8b28f7545fa6) `baec3eb` |

Sizes use the same binary units as the rest of the window. The memory figures
are ShadowCode's own estimate (see [below](#memory-estimate-and-context)) from
each file's header. Each entry records the exact file, its size and SHA-256
(`native/core/src/local_downloads.rs`). The licenses were checked in the
original model repositories: Qwen3.6, gpt-oss (plus OpenAI's one-line usage
policy to comply with applicable law) and Granite 4.2 are Apache-2.0, and
Gemma 4, unlike earlier Gemma releases, is Apache-2.0 too. Llama models are
not listed because their license is not permissive.

### The recommendation

The runtime's device list and `/proc/meminfo` decide it. ShadowCode
recommends:

1. the strongest model whose estimate fits the graphics card with 1 GB to
   spare, otherwise
2. the strongest model that runs well on a processor (Granite 3B, Gemma 4
   E4B, gpt-oss and Qwen3.6, which are small or mixture-of-experts) and leaves
   3 GB of memory for other apps, otherwise
3. the smallest model that fits with a shorter context (*Just fits. Close
   other apps while you use it*).

With less memory than that, nothing is recommended, and the first run
suggests an OpenRouter key or a subscription. A model whose architecture is
missing from the runtime's `architectures.txt` is neither recommended nor
downloadable. Each row also says how the model would run here: on the
graphics card, on the processor (slower), just fits, or needs more memory.

### How downloads work

- **Only on request.** Nothing downloads until you choose **Download** (or
  **Download** on the first run). Offline mode refuses downloads, and turning
  it on stops a running one within a few seconds (Resume continues it once
  you are back online).
- **Checked before it starts.** One model downloads at a time. The free space
  must cover what is left to download plus 512 MB; otherwise the download is
  refused with the folder and the amounts.
- **Pause, Resume, Cancel.** Pause keeps the partial file
  (`<file>.part`); Resume continues with an HTTP range request, also after
  ShadowCode restarts, and re-reads the part already on disk first. A dropped
  or stalled connection (60 seconds without data) keeps the partial file for
  Resume. Cancel deletes it. If the server ignores the range, the download
  starts over.
- **Verified.** The file gets its final name only after its size and SHA-256
  match the pinned values. A file that doesn't match is deleted, with a
  message saying so.
- **Where.** `~/.local/share/shadow-agent/local-models` (private to your
  user). Finished files join **Your models** and the picker by themselves,
  under the model's name; `config.yaml` doesn't change. **Delete** removes the
  file (unloading it first; refused while a task uses it). **Remove**, which
  never deletes, is not offered for downloaded models.
- **Network.** Hugging Face's `resolve` URL at the pinned commit, which
  redirects to its CDN. The system proxy settings (`https_proxy`) apply.

### Checking the list

```sh
# Commit, size and SHA-256 as Hugging Face reports them, plus the
# architecture and memory estimate from each file's header (range request).
cargo test -p shadowcode-core --lib catalog_matches_hugging_face -- --ignored

# Download one model with pause and resume into a scratch folder.
SHADOWCODE_LIVE_DOWNLOAD_DIR=/tmp/models SHADOWCODE_LIVE_DOWNLOAD_ID=granite-4.2-3b \
  cargo test -p shadowcode-core --lib live_download_pause_resume_verify -- --ignored
```

The list was checked against the architectures of the llama.cpp commit in
[`tools/llama.cpp.pin`](../tools/llama.cpp.pin) (`granite`, `gemma4`,
`gpt-oss`, `qwen35moe`). A unit test fails when the pin moves, so the list is
checked again before a release with a new runtime.

Checked on 2026-09-28 with that runtime on an RTX 5060 Ti (16 GB, Vulkan):
Granite 4.2 3B, Gemma 4 E4B and gpt-oss 20B were downloaded through
ShadowCode (each paused and resumed once, SHA-256 verified), loaded fully on
the GPU, and each fixed a small Python module with its tools and ran the
module's tests at the default settings (16K context). The two larger models
were checked from their headers (architecture, tool template, memory) but not
downloaded.

## Add models

In **Settings › Local models › Add a model**, choose a GGUF file or a folder.
The catalog lists at most 256 files. Removing a folder's file from the list
excludes it and leaves it on disk. Projector, vocabulary-only and embedding
GGUFs are never listed as models, and adding one directly is refused with the
reason.

### Import from Ollama

**Found in Ollama** lists the models in an existing Ollama store. ShadowCode
looks for the store in `OLLAMA_MODELS`, then `Environment=OLLAMA_MODELS` in the
Ollama systemd user unit, then `~/.ollama/models`. The first of these that has
manifests wins.

- **Import registers paths.** ShadowCode records the paths of the model blob
  and any projector blob. It never copies them and never writes to the store,
  and the Ollama daemon doesn't need to run.
- **Incompatible tags** are shown with the reason, for example
  `unsupported architecture gptoss`, and can't be imported. That one is
  Ollama's own gpt-oss format: its blob declares the architecture `gptoss`,
  while upstream GGUF files use `gpt-oss`, which the bundled runtime runs.
  Download gpt-oss 20B from the [list](#download-a-free-model) instead.
- **Other stores.** A store in another location (such as a system-wide
  `/usr/share/ollama` install) is found only if `OLLAMA_MODELS` points to it.

## What ShadowCode reads from a GGUF

Capabilities come from the file's header, never from the file name.

- **Name.** The row is named after the model's own `general.name`, plus the
  quantization from the file name when there is one (for example
  *Qwen3 14B · Q4_K_M*). A file without a name shows its file name; Ollama
  imports show their tag.
- **Compatibility.** The architecture must appear in the runtime's
  `architectures.txt`, and the model must have a token embedding tensor.
- **Tools.** The chat template must support tool calls. If it doesn't, the row
  is **Chat only**: no tool schemas are sent, so the model can answer but can't
  read or change files.
- **Thinking switch.** If the template has an `enable_thinking` switch, it is
  sent as `false`.
- **Vision.** The model needs a paired projector (mmproj). Pairing is by name
  (`<model>.mmproj.gguf`, `<model>-mmproj.gguf`, `mmproj-<model>.gguf`,
  `<model>.mmproj`), or when a folder holds exactly one model and one
  projector. Either way, the projector must match the model. Once the model is
  loaded, the Vision badge follows what the server reports in `/props`. Vision
  rows accept image attachments and get a `view_image` tool. Rows without
  vision refuse images before a job is created.

## Memory estimate and context

Each row shows an estimate: weights, KV cache, compute buffers, projector and
overhead. The KV cache follows the model's per-layer KV heads and
sliding-window layers.

- **Context.** ShadowCode starts from the lower of the trained context and
  `local_engine.context_size` (default 16,384 tokens). It halves the context
  until the estimate fits the GPU (leaving 1 GiB of VRAM free), otherwise RAM
  (leaving 2 GiB free), but not below 4,096 tokens unless the model's own
  context is smaller.
- **One number.** The result is used both as the server's `--ctx-size` and as
  the agent's context limit.
- **What the context changes.** With 32K or more, the model gets ShadowCode's
  complete tool descriptions; below that, short ones, so the answer keeps room.
  At 4K only the core coding tools are offered. When a long conversation must
  shrink, a model with 8K or more writes a short summary of the removed steps;
  smaller ones keep a built-in digest. Local models cost nothing, and their
  token counts are recorded like any other model's.
- **Fit.** The row reports whether the model fits the GPU, fits only in RAM
  (CPU), or doesn't fit at all (*Not enough memory on this computer*).

## Tool calls from smaller models

Smaller models sometimes get a tool call slightly wrong. ShadowCode repairs
the common slips instead of failing the turn, and notes each repair in the
conversation:

- arguments wrapped in a code fence, with trailing commas, single quotes,
  raw line breaks inside strings, or encoded twice;
- a call written as text instead of a real tool call: Qwen and Hermes
  `<tool_call>{…}</tool_call>`, Llama `<|python_tag|>{…}` and
  `<function=name>{…}</function>`, or an answer that is only one JSON call.
  Only tools offered in that request count, and only for models served by
  another program on this computer (Ollama, LM Studio, vLLM …). The bundled
  runtime already reads each model's own call format through its template,
  so from it, text that looks like a call (a quoted file, for example) never
  runs;
- an edit whose "old text" differs from the file only in line endings,
  spaces at line ends or indentation is applied when it matches exactly one
  place, re-indented to fit, and the agent is told to check the result.

## Loading

Choose **Load**, or just start a task on the row. ShadowCode starts one server:

```text
llama-server -m <gguf> --host 127.0.0.1 --port <free> --no-webui --jinja \
  --ctx-size <N> --parallel 1 [--mmproj <projector>] [-ngl 999]
```

- **Server security.** The environment is cleared except for an allow-list,
  and a new random key is passed as `LLAMA_API_KEY`. The key is never in argv
  and never stored. The web UI is off.
- **GPU placement.** If the estimate fits the GPU, all layers are offloaded
  (`-ngl 999`). If it fits only in RAM and a GPU exists, llama.cpp offloads what
  it can. Without a GPU, the server runs on the CPU (`--device none -ngl 0`).
- **CPU fallback.** If the server exits during a GPU start, it is retried once
  on the CPU. The row then says *Loaded · CPU fallback (GPU load failed)*.
  Otherwise the last error, including the server's own stderr, is shown on the
  row. A server that could not listen because another program took its port
  is not a GPU failure: it is started again on another free port (three tries
  in all) on the same device.
- **Out of memory.** The estimate uses the whole GPU, so another program that
  holds graphics memory can still make a load fail. ShadowCode recognises
  llama.cpp's allocation errors (CUDA, ROCm, Vulkan, Metal, SYCL and system
  memory, for the weights, the KV cache or the compute buffers) anywhere in
  the load log. When the GPU runs out on an ordinary task, the model runs on
  the CPU instead and the conversation says so (*Loaded · CPU fallback (not
  enough free GPU memory)*). When nothing fits (or in a Compare lane, which
  never falls back), the task stops before the model sees it, nothing in the
  project changes, and the conversation says what ran out and what to do:
  **Use a … -token context and retry** saves half the context as
  `local_engine.context_size` and sends the message again; **Choose another
  model** and **Open Local models** are next to it. The server's own output is
  kept in the task's result.
- **Cancel.** Loading waits up to 90 seconds for `/health` and can be
  cancelled with **Unload**.
- **One at a time.** Only one model is loaded. A task holds the loaded model
  until it ends. While any task holds it, **Load**, **Test** or **Unload** of
  another model is refused at once with "A running task is using …" (a task
  started on another model waits its turn instead). While another model is
  still loading, **Load** and **Test** say so and can be tried again. Local
  tasks in different projects take their turns in the order they were sent;
  a local follow-up still queued behind other work in its own project does
  not hold up a local task in another project.
- **Slow prompts.** On a CPU-only machine the server can take minutes to read
  a long prompt before it writes the first word. ShadowCode waits up to 15
  minutes for that first word; once the answer is streaming, 120 seconds
  without any output counts as a stall.
- **Stop.** Unload, a switch or quitting ShadowCode sends SIGTERM to the
  server's process group, then SIGKILL after 5 seconds. The server also dies
  with ShadowCode (`PR_SET_PDEATHSIG`).

## Web tools

With **Web** on for a task and the network mode *Online*, local models can use
`web_fetch` (a readable-text version of a page) and `web_search` (DuckDuckGo's
HTML results, at most 8). Results list their sources in the activity timeline.
Private, loopback, link-local and metadata addresses are blocked. To let the
agent reach a local dev server, add its exact `host:port` to
`network.allow_local_dev`. Full rules: [SECURITY.md](../SECURITY.md#web-tools).

If DuckDuckGo shows a bot check or an error, `web_search` asks Marginalia
Search's public API instead. If that fails too, it returns `blocked: true`
with both reasons and no results, and the model is told that nothing was
retrieved.

## Embedding models for code search

The same runtime also serves the optional embedding models behind
`search_code`'s search by meaning (BGE small, 35 MB, or Nomic Embed Text,
139 MB). They are separate from your chat models: installed only from
**Settings › Code intelligence**, verified against a pinned SHA-256, and run
as a second `llama-server --embedding` process on 127.0.0.1 that stops after
5 idle minutes. Loading or unloading a chat model does not affect it. See
[code intelligence](CODE_INTELLIGENCE.md#embedding-models).

## Voice models

Dictation uses whisper.cpp models (Whisper base English 141 MB, tiny English
74 MB, base multilingual 141 MB). They are not GGUF chat models and do not use
the llama.cpp runtime: whisper.cpp is compiled into ShadowCode and runs them
on the CPU. They are installed only from **Settings › Voice**, pinned to a
Hugging Face commit and verified against a SHA-256, like the embedding models.
See [voice input](VOICE.md).

## Configuration

These settings are the `local_engine` section of `config.yaml`. **Settings ›
Local models** writes the lists for you when you add, remove or import models.
Downloaded models are not listed here; they are found in the downloads folder.
`llama_binary` can only be set in the file or with `shadowcode config`;
`context_size` also changes when you choose **Use a … -token context and
retry** after a model ran out of memory.

```yaml
local_engine:
  directories: []    # folders to scan
  files: []          # individual GGUF files
  imports: []        # [{path, mmproj, name, source: ollama}] from Ollama imports
  excluded: []       # files removed from the list but still in a directory
  llama_binary: ""   # explicit llama-server path (optional)
  context_size: 0    # upper bound for the context; 0 = 16384
```

## Building the runtime yourself

[`scripts/build-llama.cpp.sh`](../scripts/build-llama.cpp.sh) builds the commit
in [`tools/llama.cpp.pin`](../tools/llama.cpp.pin) without root. The
prerequisites are in the [README](../README.md#build-from-source).
