# Code intelligence

ShadowCode's own agent loop (local and OpenRouter models) gets four kinds of
help finding its way around a project. Everything runs on this computer, and
nothing is downloaded unless you click **Install** in **Settings › Code
intelligence**.

| Part | What it does | Needs |
| --- | --- | --- |
| Language servers | Report errors right after each edit, answer "go to definition" and "find references" | A language server for the file's language |
| Code index | Definitions, references and callers for Rust, TypeScript/JavaScript, Python, Go, C, C++ and Java | Nothing (built in) |
| Repository map | A ranked outline of the code in the agent's prompt, and the `repo_map` tool | Nothing (built in) |
| Code search | `search_code`: keyword (BM25) search, plus search by meaning with an embedding model | Nothing; an embedding model is optional |

Subscription agents (Claude Code, Codex, Gemini, Antigravity) use their own
tools and are not affected.

## Language servers

When the agent reads or edits a file, ShadowCode starts a language server for
that language and project and keeps it running. Files the agent reads are
opened in it; files it edits are sent to it after every change.

After `edit_file`, `write_file` or `apply_patch`, the tool result carries a
`diagnostics` section listing the **errors the edit introduced**. Errors that
were already there are left out, even if the edit moved them to other lines,
so the agent is not sent chasing old problems. If an edit breaks another open
file (for example by changing a function's arguments), that file's new errors
are listed too. At most 12 errors are shown.

ShadowCode waits up to `diagnostics_wait_ms` (3 seconds) per edit. If the
server is still loading the project (rust-analyzer on a large workspace can
take a while) or answers later, the edit is not held up: the result says the
check is pending, and the agent can call `get_diagnostics` on the file later.

| Language | Server | Found |
| --- | --- | --- |
| Rust | `rust-analyzer` | PATH, `~/.cargo/bin` (`rustup component add rust-analyzer`) |
| TypeScript, JavaScript | `typescript-language-server` | Managed install, PATH |
| Python | `basedpyright-langserver` or `pyright-langserver` | Managed install, PATH, `~/.local/bin` |
| Go | `gopls` | PATH, `~/go/bin` |
| C, C++ | `clangd` | PATH |

**Settings › Code intelligence** shows which servers were found and where.

### What a server may do

A server runs whenever the agent edits a file, without an approval of its
own, so ShadowCode starts servers in a way that does not build or run
project code and does not reach the network:

- **rust-analyzer**: build scripts, procedural macros and `cargo check` are
  off; cargo runs offline. Errors come from rust-analyzer's own analysis
  (syntax, names, types), not from the compiler.
- **typescript-language-server**: always uses the TypeScript installed next to
  the server, never `node_modules/typescript` from the project. Automatic type
  downloads are off. If no TypeScript is installed next to the server, the
  server is not used; install the managed one instead.
- **gopls**: `GOTOOLCHAIN=local` and `GOPROXY=off`, so no toolchain or module
  downloads.
- **clangd**: no background index (it would write `.cache/` into the project).
- **pyright**: checks open files only.

Servers get a short list of environment variables (PATH, HOME, locale, and
the toolchain variables such as `CARGO_HOME`, `GOPATH` or `VIRTUAL_ENV`); API
keys and other secrets in ShadowCode's environment are not passed on. They
run in their own process group and stop when ShadowCode stops. Server edits
(`workspace/applyEdit`) are always refused: only the agent's tools change
files.

### Lifetime and limits

- One server per project and language. At most `max_servers` (4) run at once;
  starting another stops the one idle the longest.
- A server idle for `lsp_idle_minutes` (10) stops.
- A server that stops reading what ShadowCode sends (hung) is stopped after
  10 seconds; the edit goes ahead and says the file could not be checked.
- A server that crashes restarts on next use after 2, 4, 8 … seconds (at most
  5 minutes). The status page shows the last error.
- Memory held per server is bounded: 48 open files (the oldest is closed),
  diagnostics for 1,024 files, 200 per file, 8 KB of the server's own error
  output.
- **Stop all** in Settings stops every server now.

### Managed installs

For TypeScript/JavaScript and Python, **Settings › Code intelligence** offers
**Install**, which runs npm (Node.js with npm must be installed) into
ShadowCode's own folder, `~/.local/share/shadow-agent/code-intel/language-servers/`.
Versions are pinned and npm install scripts are disabled.

| Install | Packages | Size |
| --- | --- | --- |
| TypeScript / JavaScript | `typescript-language-server@5.3.0`, `typescript@5.9.3` | about 25 MB |
| Python | `pyright@1.1.414` | about 19 MB |

**Remove** deletes that folder. Offline mode disables Install.

## Code index

The index (tree-sitter parsing into ordinary SQLite tables) covers Rust,
TypeScript/TSX, JavaScript, Python, Go, C, C++ and Java. It records each
file's definitions, how often each name is used in each file, and the file
cut into chunks for search. Other text files (Markdown, TOML, YAML, JSON,
shell scripts, SQL and similar) are indexed for search only.

- It is built on first use. A scan walks every indexable file (up to
  250,000) but parses at most 2,000 changed files at a time, so a large
  repository fills in over a few passes while each one stays short.
  `node_modules`, `target`, `dist`, `build`, `vendor`, hidden folders,
  ignored files and secret files are skipped, as are files over 512 KB.
- Unchanged files (same modification time and size) are not read again.
  Files the agent edits are re-indexed right away.
- The index is kept between runs in the profile's cache
  (`$XDG_CACHE_HOME/shadow-agent/index`, or `<profile>/cache/index` for an
  isolated profile), never in the project. An index from another version of
  ShadowCode, or a damaged one, is rebuilt.
- **Settings › Code intelligence** shows the files indexed out of those
  found, whether the scan is complete, and the index's size. **Reindex this project**
  continues a large scan (up to 90 seconds per click). A **focus folder**
  limits the index to one folder of the project, useful in a monorepo.
  A project with more files than a scan follows (250,000, or a million
  folder entries) says so and asks for a focus folder; its scan is reused
  for a minute instead of walking the project again at every task.
  **Clear index** deletes the project's index; it is rebuilt when needed.
  The index of a worktree task, Compare lane or automation run is deleted
  when its worktree is removed.

Tools that use it: `workspace_symbols`, `goto_definition` and
`find_references` by name, `get_type_signature`, and the callers list after
edits. `goto_definition` and `find_references` with a `path` and `line` (and
optionally `column`) ask the language server first, which understands types,
and fall back to the index.

## Repository map

At the start of each task, the agent's system prompt gets a short outline of
the project's most important definitions, like this:

```
src/store.rs:
   42│ pub struct Store {
   88│     pub fn open(path: &Path) -> Result<Self> {
src/engine.rs:
  120│ pub async fn run(&self, job: Job) -> Result<()> {
```

Definitions are ranked by how the code uses them: files are linked to the
files whose names they use, and PageRank over those links decides what
matters most. Files you name in the task (`fix the bug in store.rs`), files
the agent edited recently, and names mentioned in the task rank higher.
Line numbers come from the index and can be out of date, so the agent still
reads files before editing.

The map is sized by `repo_map_tokens` (1,024 by default, 0 turns it off) and
never takes more than 1/32 of the model's context window; models with 8,192
tokens of context or less get no map. A task waits at most 4 seconds for it:
in a large project the first task may start without a map while the index
builds, and later tasks get one. The agent can ask for a larger or
focused map with the `repo_map` tool (`query`, `paths`, `max_tokens`).

## Code search

`search_code` (`query`, optional `path` prefix, `max_hits`) returns ranked
chunks of code with their line ranges and a short excerpt.

- **Keywords (always).** SQLite FTS5 with BM25 ranking. Query words are
  matched as prefixes; `parseConfig`, `parse_config` and "parse config" all
  find each other. Definition names and file paths weigh more than body text.
- **Meaning (optional).** With an embedding model installed, each chunk also
  gets a vector, and the query is compared by cosine similarity. The two
  rankings are merged with reciprocal rank fusion (RRF). Without a model, or
  if the embedding server fails, search uses keywords alone and says so.

### Embedding models

| Model | Size | License | Notes |
| --- | --- | --- | --- |
| BGE small (English) v1.5, Q8_0 | 35 MB | MIT | Smallest and fastest; reads the first ~1,200 characters of a chunk |
| Nomic Embed Text v1.5, Q8_0 | 139 MB | Apache-2.0 | Larger; reads whole chunks |

Models download from Hugging Face only when you click **Install**. Each URL
is pinned to a repository commit, and the file must match its recorded size
and SHA-256 before it is kept. Files go to
`~/.local/share/shadow-agent/code-intel/models/`.

Embeddings run on the bundled llama.cpp as a second, separate
`llama-server --embedding` process (the chat model keeps its own), bound to
127.0.0.1 with a random key. It tries the GPU and falls back to the CPU, and
stops after 5 idle minutes.

After an install, the open project is embedded in the background. Each
search also embeds up to 6 seconds' worth of chunks that have no vector yet,
so results improve as it goes; the result reports how many chunks have
vectors. Vectors are stored in
`~/.local/share/shadow-agent/code-intel/vectors.sqlite`, keyed by the chunk's
content, so unchanged code is never embedded twice (at most 60,000 vectors
per model; the oldest go first).

## Settings

The `code_intel` section of `config.yaml`. A project's own
`.shadow/config` cannot change it.

```yaml
code_intel:
  lsp: true                  # start language servers
  diagnostics_on_edit: true  # attach new errors to edit results
  diagnostics_wait_ms: 3000  # 200-30000
  lsp_idle_minutes: 10       # 1-240
  max_servers: 4             # 1-16
  servers: {}                # per language: {command, args}
  repo_map_tokens: 1024      # 0 turns the map off; at most 16384
  semantic_search: true      # use an installed embedding model
  embedding_model: ""        # an installed model id; empty = first installed
```

To use a server ShadowCode does not find, name it per language (`rust`,
`typescript`, `python`, `go`, `c`):

```yaml
code_intel:
  servers:
    python:
      command: /opt/tools/bin/pylsp
      args: []
```

Set `SHADOWCODE_LSP_TRACE=1` to print every message a server sends to
ShadowCode's standard error.

## Troubleshooting

- **No errors are ever attached.** Check that the language shows a server in
  Settings, and that **Tell the agent about errors its edits introduce** is
  on. A pending check means the server was still loading.
- **TypeScript says it needs TypeScript beside the server.** Your
  `typescript-language-server` has no TypeScript installed next to it.
  Install the managed one, or `npm install -g typescript`.
- **rust-analyzer reports nothing for macro-heavy code.** Procedural macros
  are off on purpose (they would run project code). Run `cargo check` for
  the full compiler picture.
- **Semantic search says it failed.** Read the error in the result or in
  Settings; the llama.cpp runtime must be installed (see
  [local models](LOCAL_MODELS.md)).
