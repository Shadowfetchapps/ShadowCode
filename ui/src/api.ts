import { request, ApiError } from "./lib/transport";
import { createVerificationReader } from "./lib/verificationRefresh";
import type { BillingMode, PickerTarget, UsageSnapshot } from "./lib/picker";
import type { ContextAttachment } from "./lib/pendingAttachments";
import type { PreviewOpened, PreviewServer } from "./lib/preview";

// --- Picker, accounts, local models (docs/API_CONTRACT.md)

export type VendorModel = {
  id: string;
  label: string;
  is_default?: boolean;
  vision?: boolean;
};

export type EditorRecoveryDraft = {
  path: string;
  base: string;
  draft: string;
  base_hash: string;
  revision: string;
  updated_at: number;
};

export type VendorStatus = {
  billing?: BillingMode | null;
  /** Product name without the CLI binary, e.g. "Claude Code". */
  product?: string;
  id: string;
  label: string;
  state: "ready" | "not_logged_in" | "not_installed" | "unavailable" | string;
  status?: string;
  availability: string;
  availability_label: string;
  detail?: string;
  version?: string | null;
  binary?: string | null;
  fix?: string | null;
  account?: {
    email?: string | null;
    plan?: string | null;
    auth_mode?: string | null;
  } | null;
  models?: VendorModel[];
  accepts_images?: boolean;
  asks_approval?: boolean;
  fetched_at?: number | null;
  error?: string | null;
  usage_note?: string | null;
  login_command?: string[];
  logout_command?: string[];
  shared_cli_note?: string;
  usage?: UsageSnapshot | null;
  /** Antigravity only: Google's ACP agent server, installed on request. */
  install?: AgentInstall | null;
};

/** GET /api/accounts/antigravity/install: the agent server ShadowCode
 * downloads only when the user chooses Install. */
export type AgentInstall = {
  installed: boolean;
  version: string;
  /** The server in use, or null when none is installed. */
  path: string | null;
  /** The copy in use is the one ShadowCode installed (and can remove). */
  managed: boolean;
  download_bytes: number;
  installed_bytes: number;
  /** Download URL (dl.google.com). */
  source: string;
  /** Folder the managed copy is unpacked into. */
  dir: string;
  state:
    | "not_installed"
    | "downloading"
    | "verifying"
    | "unpacking"
    | "installed"
    | "error"
    | string;
  busy: boolean;
  /** Bytes downloaded so far and the expected total. */
  done: number;
  total: number;
  error: string | null;
};

export type MemoryEstimate = {
  weights_bytes: number;
  kv_cache_bytes: number;
  compute_bytes?: number;
  projector_bytes: number;
  overhead_bytes?: number;
  total_bytes: number;
  context_tokens?: number;
};

export type GgufEntry = {
  id: string;
  name: string;
  path: string;
  bytes: number;
  /** `download`: fetched from the built-in catalog (Delete frees the disk). */
  source: "file" | "directory" | "ollama" | "download" | string;
  architecture: string | null;
  context_train: number | null;
  context_tokens: number;
  compatible: boolean;
  reason: string;
  vision: boolean;
  mmproj: string | null;
  /** Current schema-offering policy, not verified tool/coding capability. */
  tools: boolean;
  tools_reason?: string;
  tools_basis?:
    | "unknown"
    | "known_template_profile"
    | "template_hint"
    | "no_template_hint"
    | "no_template";
  memory?: MemoryEstimate | null;
  fits?: "gpu" | "cpu" | "no" | string;
  availability?: string;
  last_error?: string | null;
};

/** How a catalog model runs on this computer (GET /api/local-models/downloads). */
export type DownloadFit = "gpu" | "cpu" | "tight" | "no";
export type DownloadState =
  "available" | "downloading" | "checking" | "paused" | "failed" | "installed";

/** A free model ShadowCode can download, pinned to one Hugging Face commit. */
export type DownloadModel = {
  id: string;
  name: string;
  publisher: string;
  summary: string;
  file: string;
  bytes: number;
  sha256: string;
  license: string;
  license_url: string;
  source_url: string;
  quantization: string;
  architecture: string;
  /** Memory needed at the default context, and at the smallest one. */
  memory_bytes: number;
  min_memory_bytes: number;
  fit: DownloadFit;
  recommended: boolean;
  /** The bundled llama.cpp can load this architecture. */
  supported: boolean;
  unsupported_reason?: string | null;
  state: DownloadState;
  done: number;
  total: number;
  bytes_per_second: number;
  error?: string | null;
  /** The local picker id once downloaded. */
  model_id?: string | null;
  path?: string | null;
};

export type DownloadCatalog = {
  directory: string;
  free_bytes: number | null;
  offline: boolean;
  hardware: {
    ram_bytes: number;
    vram_bytes: number | null;
    gpu: string | null;
  };
  recommended: string | null;
  recommended_fit: DownloadFit | null;
  /** A download or resume check is running. */
  busy: boolean;
  models: DownloadModel[];
};

export type OllamaModel = {
  tag: string;
  path: string;
  projector: string | null;
  bytes: number;
  compatible: boolean;
  reason: string;
  already_added: boolean;
};

/** Settings › Code intelligence (`/api/code-intel/*`). */
export type CodeIntelSettings = {
  lsp: boolean;
  diagnostics_on_edit: boolean;
  diagnostics_wait_ms: number;
  lsp_idle_minutes: number;
  max_servers: number;
  repo_map_tokens: number;
  semantic_search: boolean;
  embedding_model: string;
  servers: Record<string, { command: string; args: string[] }>;
};
export type CodeIntelLanguage = {
  language: string;
  label: string;
  available: boolean;
  enabled: boolean;
  server?: string;
  path?: string;
  source?: "config" | "managed" | "path";
  note?: string;
  install_hint?: string;
  managed_package?: string | null;
};
export type CodeIntelManaged = {
  id: string;
  label: string;
  packages: string[];
  approx_bytes: number;
  installed: boolean;
  installed_bytes: number | null;
  progress: { state: string; error?: string | null } | null;
};
export type EmbeddingModel = {
  id: string;
  name: string;
  summary: string;
  bytes: number;
  license: string;
  installed: boolean;
  active: boolean;
  progress: {
    state: string;
    done: number;
    total: number;
    error?: string | null;
  } | null;
};
export type CodeIntelStatus = {
  config: CodeIntelSettings;
  config_error?: string | null;
  offline: boolean;
  languages: CodeIntelLanguage[];
  servers: {
    root: string;
    language: string;
    server?: string;
    state: string;
    last_error?: string | null;
  }[];
  managed: CodeIntelManaged[];
  npm: { available: boolean; path?: string | null; node?: string | null };
  index: {
    files: number;
    symbols: number;
    chunks: number;
    languages: Record<string, number>;
  } | null;
  embeddings: {
    models: EmbeddingModel[];
    active: string | null;
    runtime: string | null;
    coverage: { embedded: number; chunks: number } | null;
    backfill: { state: string; embedded: number; error?: string | null } | null;
  };
};
export type LocalCatalog = {
  hardware?: {
    cpu_cores?: number;
    ram_bytes?: number;
    gpu?: string | null;
    vram_bytes?: number | null;
    backend?: "vulkan" | "cpu" | "unknown" | string;
    devices?: string[];
    detail?: string;
  };
  runtime?: {
    state: "ready" | "setup_required" | "unavailable" | string;
    path?: string | null;
    origin?: string;
    version?: string | null;
    backend?: string | null;
    commit?: string | null;
    detail?: string;
  };
  models?: GgufEntry[];
  loaded?: {
    id: string;
    name: string;
    port?: number;
    since?: number;
    context_tokens?: number;
    backend?: string;
  } | null;
  ollama_store?: {
    path?: string | null;
    available: boolean;
    models: OllamaModel[];
  };
};

export type PickerResponse = {
  targets: PickerTarget[];
  local_engine?: LocalCatalog;
  vendors?: Record<string, VendorStatus>;
  generated_at?: number;
};

export type AccountsResponse = {
  vendors: Record<string, VendorStatus>;
  config?: Record<string, unknown>;
  local_engine?: LocalCatalog;
};

/** GET /api/openrouter: the saved API key's state (never the key itself) and
 * the cached model list. */
export type OpenRouterStatus = {
  key_set: boolean;
  key: {
    label: string;
    /** USD used by this key. */
    usage: number;
    limit: number | null;
    limit_remaining: number | null;
    is_free_tier: boolean;
    /** Account balance in USD (credits bought minus used), when reported. */
    credits_remaining?: number | null;
  } | null;
  key_error: string | null;
  models: number;
  tool_models: number;
  /** Unix seconds of the last model-list fetch. */
  fetched_at: number | null;
  offline: boolean;
  keys_url: string;
  activity_url: string;
};

/** GET /api/allowance: how much each way of running a model has left, only
 * from what each source reports (no estimates). */
export type AllowanceState =
  | "ok"
  | "low"
  | "limit_reached"
  | "unknown"
  | "sign_in"
  | "not_installed"
  | "unavailable"
  | "offline"
  | "no_key"
  | "none";

export type AllowanceWindow = {
  label: string;
  remaining_percent: number | null;
  /** Unix seconds. */
  resets_at: number | null;
};

export type AllowanceRow = {
  /** "cli:codex" …, "openrouter" or "local". */
  id: string;
  kind: "subscription" | "api_key" | "local" | string;
  product: string;
  state: AllowanceState | string;
  headline: string;
  remaining_percent: number | null;
  windows?: AllowanceWindow[];
  plan?: string | null;
  note?: string | null;
  /** Unix seconds of the vendor's last usage check. */
  last_checked?: number | null;
  usage_url?: string | null;
  /** OpenRouter only, USD. */
  used?: number | null;
  limit?: number | null;
  limit_remaining?: number | null;
  /** Local only. */
  ready_models?: number;
  on_limit?: "local" | "ask" | string;
  fallback?: { id: string; name: string } | null;
};

export type AllowanceResponse = { generated_at: number; rows: AllowanceRow[] };

/** config `limits`: what happens when a subscription reports its plan limit. */
export type LimitsConfig = {
  on_limit?: "local" | "ask" | string;
  /** "" = automatic, or a `local:gguf:` picker id. */
  fallback_model?: string;
};

export type LoginProgress = {
  running: boolean;
  cancellation_requested?: boolean;
  lines: string[];
  done: {
    ok: boolean;
    detail?: string;
    availability?: string;
    availability_label?: string;
  } | null;
};

export type ConsentRequest = {
  error?: string;
  needs_consent: true;
  handoff: {
    from?: string | null;
    to?: string | null;
    excerpt_chars?: number;
    images?: number;
  };
};

/** An explicit user-selected check, executed locally without a model turn. */
export type CheckJobRequest = {
  workspace: string;
  session_id: string;
  command: string;
  timeout: number;
  queue: false;
};

export type StartJobRequest = {
  task: string;
  workspace?: string;
  session_id?: string;
  model?: string;
  purpose?: string;
  queue?: boolean;
  images?: string[];
  web?: boolean;
  handoff_consent?: boolean;
  /** Reasoning effort (`low`, `medium`, `high`); omitted keeps the model's
   * default. Only for rows whose picker entry has `reasoning`. */
  effort?: string;
  /** Files and folders the message @-mentions. */
  mentions?: { path: string; kind: "file" | "dir" }[];
  /** Elements and console messages from the Preview tab; the engine appends
   * their `text` after the task (docs/PREVIEW.md). */
  context?: ContextAttachment[];
  /** Start a new conversation in a fresh worktree of the project; it runs
   * beside a task in the main checkout. */
  worktree?: boolean;
};

/** A conversation run in its own managed worktree ("Run in new worktree"),
 * GET /api/worktree-tasks/{id}. */
export type WorktreeTask = {
  id: string;
  /** The project (main checkout). */
  workspace: string;
  session_id: string;
  worktree: string;
  branch: string;
  base: { commit: string; head: string; included_uncommitted: boolean };
  task: string;
  created_at: number;
  finished_at?: number | null;
  /** starting | running | done | applied | branch | discarded */
  state: string;
  job_id: string;
  status: string;
  changed_files: CompareFile[];
  changed_files_truncated: boolean;
  applied_files: string[];
  /** Files `git apply --check` refused on the last apply (nothing written). */
  conflicts: string[];
  conflict_detail: string;
  kept_branch?: string | null;
  notes: string[];
  removed: boolean;
};

/** The engine's per-task token and cost accounting (API contract "Usage"). */
export type Usage = {
  prompt_tokens?: number;
  completion_tokens?: number;
  total_tokens?: number;
  cached_tokens?: number;
  cache_write_tokens?: number;
  cost_usd?: number | null;
  cost_estimated?: boolean;
  estimated?: boolean;
  source?: string;
  turns?: number;
};

function consentFrom(value: unknown): ConsentRequest | null {
  if (
    value &&
    typeof value === "object" &&
    (value as { needs_consent?: unknown }).needs_consent === true
  ) {
    const body = value as ConsentRequest;
    return { ...body, handoff: body.handoff || {} };
  }
  return null;
}

export type Project = {
  id: string;
  path: string;
  name: string;
  last_opened: number;
};
export type Session = {
  id: string;
  workspace: string;
  status: string;
  title?: string;
  updated_at: number;
  usage_json?: string;
  parent_id?: string;
  /** Set on a Compare lane's conversation (hidden from the sidebar). */
  compare_id?: string | null;
  /** The lane's picker id. */
  compare_lane?: string | null;
  /** Set while the conversation runs in its own worktree. */
  worktree_task?: string | null;
  /** The project a worktree conversation belongs to (listed under it). */
  worktree_source?: string | null;
};
export type SessionDetail = Session & {
  /** The open worktree task this conversation runs in (as last saved). */
  worktree?: WorktreeTask | null;
  /** Sum of the conversation's finished jobs. */
  usage?: Usage;
  tasks: { id: string; prompt: string; summary?: string; status: string }[];
  events: EventRow[];
  event_cursor: number;
  history_page?: { first_cursor: number; has_older: boolean };
  /** Picker id remembered for this conversation (POST /api/sessions/{id}/target). */
  execution_target?: string | null;
  native_sessions?: Record<string, string>;
};
export type HistoryPage = {
  events: EventRow[];
  first_cursor: number;
  event_cursor: number;
  has_older: boolean;
};
/** An agent definition for subagents (GET /api/agents). */
export type AgentInfo = {
  name: string;
  description: string;
  mode: "read-only" | "write";
  model?: string | null;
  source: "builtin" | "project" | "user";
  path: string;
};
export type EventRow = {
  id?: number;
  ts: number;
  type: string;
  payload: Record<string, unknown>;
  session_id?: string;
  task_id?: string;
};
export type ProjectSkill = {
  name: string;
  content: string;
  raw_content?: string;
  description?: string;
  path?: string;
  mode?: string;
  hash?: string;
};
export type FileEntry = { name: string; path: string; type: "file" | "dir" };
export type PlanStep = {
  id: string;
  title: string;
  status: string;
  detail?: string;
};
/** What an approval would do (approvals::preview in the engine). */
export type PreviewFile = {
  path: string;
  status: "added" | "modified" | "deleted" | string;
  /** Unified diff body: `@@` headers and `+`/`-`/space lines. */
  diff: string;
  added: number;
  removed: number;
  truncated: boolean;
  binary: boolean;
};
export type ApprovalPreview =
  | { kind: "files"; files: PreviewFile[] }
  | { kind: "command"; command: string; cwd: string }
  | { kind: "move"; from: string; to: string }
  | { kind: "folder"; path: string };
export type Approval = {
  id: string;
  session_id?: string;
  task_id?: string;
  command?: string;
  reason?: string;
  tool?: string;
  pending?: boolean;
  preview?: ApprovalPreview | null;
  /** What "Allow for this task" covers ("file edits", "`cargo test`
   * commands"); empty when the action can only be allowed once. */
  grant?: string;
  /** A note given with Deny reaches the agent. */
  note?: boolean;
};
export type ReviewFile = {
  path: string;
  status: "added" | "modified" | "deleted" | "unchanged" | "unavailable";
  /** checkpoint: compared with the file before this task; git: a vendor
   * agent's change, compared with the last commit. */
  source: "checkpoint" | "git" | "none";
  added: number;
  removed: number;
  binary: boolean;
  error?: string;
};
export type ReviewHunk = {
  id: string;
  header: string;
  lines: { kind: "add" | "del" | "ctx"; text: string; eol?: boolean }[];
};
export type ReviewFileDetail = ReviewFile & {
  hunks: ReviewHunk[];
  hash: string;
};
export type LifecycleHook = {
  name: string;
  events: string[];
  builtin: boolean;
  command?: string;
  description?: string;
  path?: string;
  hash?: string;
  enabled?: boolean;
  timeout_sec?: number;
  path_suffix?: string;
};
export type HookCatalog = {
  hooks: LifecycleHook[];
  dirs: string[];
  issues?: string[];
  format?: string;
  workspace?: string;
  trusted?: boolean;
  approved?: { path: string; hash: string }[];
};
export type ManagedWorktree = {
  id: string;
  source: string;
  path: string;
  branch: string;
  base_commit: string;
  common_directory: string;
  state: string;
  created_at: number;
  detail: string;
};
export type WorktreeRepairReview = {
  record: ManagedWorktree;
  administrative_directory: string;
  head: string;
  checkout_pointer: string | null;
  registration_pointer: string | null;
  warning: string;
  hash: string;
};
export type WorktreeCopyReview = {
  source: string;
  head: string;
  staged_diff: string;
  unstaged_diff: string;
  untracked: { path: string; bytes: number; hash: string; mode: number }[];
  intent_to_add: string[];
  hash: string;
};
export type WorktreeReturnReview = {
  record: ManagedWorktree;
  source_head: string;
  source_branch: string;
  worktree_head: string;
  worktree_branch: string;
  merge_base: string;
  diff: string;
  hash: string;
};
export type WorktreeRecovery = {
  record: ManagedWorktree;
  commit: string;
  branch: string;
  warning: string;
  hash: string;
};
export type WorktreeInspection = {
  record: ManagedWorktree;
  head: string;
  current_branch: string;
  status: string;
  can_remove: boolean;
  reason: string;
  hash: string;
};
export type TaskTimings = {
  schema_version: number;
  complete: boolean;
  total_seconds: number;
  queue_seconds: number;
  active_seconds?: number | null;
  preparation_seconds?: number | null;
  runtime_wait_seconds?: number | null;
  model_load_seconds?: number | null;
  model_reused?: boolean | null;
  model_requests_seconds?: number | null;
  model_requests: number;
  first_text_seconds?: number | null;
  first_text_request?: number | null;
  tool_batches_seconds?: number | null;
  final_checks_seconds?: number | null;
  check_process_seconds?: number | null;
};
export type Job = {
  timings?: TaskTimings | null;
  id: string;
  task_id?: string;
  workspace: string;
  event_cursor: number;
  started_at: number;
  finished_at?: number;
  session_id: string;
  status: string;
  summary?: string;
  task?: string;
  task_truncated?: boolean;
  purpose?: string;
  usage?: Record<string, number>;
  model?: string;
  mode?: string;
  routing?: RoutingDecision | null;
  web?: boolean;
  result?: {
    timings?: TaskTimings | null;
    success: boolean;
    summary: string;
    plan: { goal: string; steps: PlanStep[] };
    usage?: Record<string, number>;
    verification?: Record<string, unknown>;
    /** Set when a subscription reported its plan limit (status limit_reached). */
    limit_reached?: { vendor: string; detail?: string; usage?: unknown };
  };
  /** Set on the answer to a "Run in new worktree" start. */
  worktree_task?: WorktreeTask;
};
export type Health = {
  ok: boolean;
  desktop_attached?: boolean;
  version?: string;
  workspace: string;
  trusted?: boolean;
  permissions?: Record<string, unknown>;
  provider?: { ok: boolean; name: string; detail: string };
  tools?: Record<string, { ok: boolean; path?: string; detail?: string }>;
  model?: Record<string, unknown>;
  onboarding?: { completed: boolean };
};
export type DiffHunk = {
  header: string;
  lines: { kind: string; text: string }[];
};
export type ExecResult = {
  ok: boolean;
  command: string;
  stdout: string;
  stderr: string;
  exit_code: number;
  error?: string;
};
export type Milestone = {
  id: string;
  title: string;
  status: string;
  detail?: string;
  task_id?: string;
  require_verification?: boolean;
  mode?: string;
};
export type Goal = {
  id: string;
  workspace: string;
  instruction: string;
  title?: string;
  status: string;
  progress: number;
  progress_pct: number;
  running: boolean;
  milestones: Milestone[];
  updated_at: number;
  session_id?: string;
  job_id?: string;
  run_detail?: string;
};
export type BackgroundTask = {
  id: string;
  name: string;
  command: string;
  status: string;
  pid: number;
  exit_code: number | null;
  output: string;
  cwd?: string;
  started_at?: number;
  ended_at?: number | null;
  error?: string;
  truncated?: boolean;
  output_preview_truncated?: boolean;
};
export type RoutingDecision = {
  purpose: string;
  source: string;
  requested: string;
  model_id: string;
  model_name: string;
  provider: string;
  context_limit: number;
  fallback_reason?: string | null;
  inference?: string;
};
export type DoctorReport = {
  ok: boolean;
  version: string;
  diagnostic_export?: {
    id: string;
    filename: string;
    content: string;
    mime: string;
    captured_at: string;
    byte_length: number;
  };
  checks: {
    id: string;
    ok: boolean;
    status?: "pass" | "warn" | "fail" | "info" | "not_checked";
    label: string;
    detail?: string;
    fix?: string;
  }[];
  suggestions: string[];
};
export type McpServer = {
  name: string;
  command?: string[] | null;
  url?: string | null;
};
export type NativeMcpServer = McpServer & {
  api_key_env?: string | null;
  id: string;
  hash: string;
  description: string;
  timeout_sec: number;
  env_names: string[];
  env_refs: Record<string, string>;
  enabled: boolean;
  transport: "stdio" | "http";
};
export type NativeMcpCatalog = {
  format: "native-mcp-v1";
  servers: NativeMcpServer[];
  approved: { workspace: string; server: string; hash: string }[];
  issues: string[];
  workspace: string;
  trusted: boolean;
  dirs: string[];
};

/** docs/COMPARE.md: one task on 2–3 models, each in its own worktree. */
export type LocalFileIdentity = {
  path: string;
  bytes: number;
  modified_ns?: string | null;
  device?: string;
  inode?: string;
  changed_seconds?: number;
  changed_nanoseconds?: number;
};
export type LocalStringIdentity = { sha256: string; bytes: number };
export type LocalToolCapabilities = {
  field_status: "missing" | "invalid" | "reported";
  supports_tools: boolean | null;
  supports_tool_calls: boolean | null;
  supports_parallel_tool_calls: boolean | null;
  supports_object_arguments: boolean | null;
  invalid_fields: string[];
};
export type LocalModelProvenance = {
  schema: 1;
  identity_kind: "filesystem_metadata";
  files: {
    model: LocalFileIdentity;
    runtime: LocalFileIdentity;
    projector?: LocalFileIdentity | null;
  };
  model: {
    gguf_version?: number;
    architecture?: string | null;
    header_sha256?: string;
    header_bytes?: number;
    quantization?: {
      file_type?: number | null;
      version?: number | null;
      tensor_type_counts?: Record<string, number>;
    };
    chat_template?: LocalStringIdentity | null;
  };
  runtime: {
    reported_version?: string | null;
    reported_commit?: string | null;
    reported_generation_defaults?: Record<string, number | string[]> | null;
    reported_chat_template?: LocalStringIdentity | null;
    reported_chat_template_tool_use?: LocalStringIdentity | null;
    reported_tool_capabilities?: LocalToolCapabilities;
  };
  template_override?: {
    profile?: string;
    source?: string;
    source_commit?: string;
    source_sha256?: string;
    source_template?: LocalStringIdentity;
    template?: LocalStringIdentity;
    match_kind?: "source_exact" | "pinned_lexer_exact";
    normalization?: "none" | "single_final_lf_removed";
    runtime_template_verified?: boolean;
  } | null;
  context: { requested_tokens?: number; reported_tokens?: number | null };
  gpu: {
    requested_mode?: string;
    launch_mode?: string;
    reported_backend?: string;
  };
};
export type LocalRuntimeReceipt = {
  model_id: string;
  preparation_seconds: number;
  automatic_cpu_fallback_allowed: boolean;
  runtime?: {
    backend: string;
    context_tokens: number;
    cpu_fallback?: boolean;
    provenance?: LocalModelProvenance;
  } | null;
  request_policy?: {
    sampling_source?: string;
    sampling_overrides?: Record<string, number>;
    max_tokens_policy?: string;
    chat_template_kwargs?: { enable_thinking?: boolean } | null;
  };
};
export type CompareFile = {
  path: string;
  /** added | modified | deleted */
  status: string;
  additions: number;
  deletions: number;
  binary: boolean;
};
export type CompareLane = {
  model: string;
  name: string;
  session_id: string;
  job_id: string;
  worktree: string;
  worktree_id: string;
  branch: string;
  base_commit: string;
  /** The lane's latest job status (queued, running, paused, cancelling,
   * completed, failed, cancelled, limit_reached, interrupted). */
  status: string;
  summary: string;
  changed_files: CompareFile[];
  changed_files_truncated: boolean;
  checks: {
    passed: number;
    failed: number;
    incomplete?: number;
    commands: {
      command: string;
      exit_code: number | null;
      success: boolean;
      state?: string;
    }[];
  };
  duration_s: number;
  timings?: TaskTimings | null;
  local_progress?: { model_id: string; phase: string } | null;
  local_runtime?: LocalRuntimeReceipt | null;
  usage: {
    prompt_tokens?: number;
    completion_tokens?: number;
    total_tokens?: number;
    estimated?: boolean;
    cost?: number;
  } | null;
  error: string | null;
  removed: boolean;
};
export type CompareRecord = {
  id: string;
  workspace: string;
  task: string;
  mode: string;
  web: boolean;
  created_at: number;
  finished_at: number | null;
  state: "running" | "done" | "applied" | "discarded" | string;
  base: { commit: string; head: string; included_uncommitted: boolean };
  lanes: CompareLane[];
  winner: string | null;
  applied_files: string[];
  cleanup_pending?: boolean;
  notes: string[];
};
export type CompareScore = {
  model: string;
  name: string;
  wins: number;
  runs: number;
};
export type StartCompareRequest = {
  workspace?: string;
  task: string;
  models: string[];
  mode?: "code" | "plan" | "ask";
  web?: boolean;
};

/** GET /api/sandbox/status: shell isolation this computer provides. */
export type SandboxStatus = {
  effective: "bubblewrap" | "landlock" | "none" | "blocked";
  bubblewrap: { installed: boolean; works: boolean; detail: string };
  landlock_abi: number;
  network_namespace: { available: boolean; detail: string };
  require: boolean;
  shell_network: "off" | "on" | "allowlist";
  allow: string[];
  home_read_only: string[];
  home_skipped: string[];
  never_mounted: string[];
};

/** How this copy was installed (`/api/about`, `/api/updates`). */
export type InstallInfo = {
  kind: "appimage" | "deb" | "system" | "source" | "unknown";
  label: string;
};

/** The newest stable release GitHub reported. */
export type UpdateRelease = {
  version: string;
  tag: string;
  /** The release page with its notes and downloads. */
  url: string;
  published_at?: string | null;
  /** It carries the signature files the AppImage installer needs. */
  signed: boolean;
};

/** What to do to update this kind of installation. */
export type UpdateStep = {
  text: string;
  command: string | null;
  link: string | null;
};

/** `GET /api/updates`: the update notice and its settings. Only
 * `?auto=1` (at most once a day) and `POST /api/updates/check` reach
 * GitHub. */
export type UpdateStatus = {
  current: string;
  /** False when the build or a system policy turned checks off. */
  allowed: boolean;
  /** The daily check runs (policy and `updates.check`). */
  automatic: boolean;
  /** `updates.check` in config.yaml; null follows the packaged default. */
  setting: boolean | null;
  default_on: boolean;
  offline: boolean;
  policy_message: string | null;
  policy_source: string | null;
  install: InstallInfo;
  last_checked_at: number | null;
  last_attempt_at: number | null;
  error: string | null;
  latest: UpdateRelease | null;
  available: boolean;
  dismissed: boolean;
  next_step: UpdateStep | null;
  releases_url: string;
};

/** `GET /api/about`: Settings › About. */
export type AboutInfo = {
  name: string;
  version: string;
  commit: string | null;
  install: InstallInfo;
  license: {
    spdx: string;
    name: string;
    holder: string;
    /** ShadowCode's NOTICE file. */
    notice: string;
    /** Folder with third-party license texts, when installed. */
    third_party: string | null;
  };
  links: {
    repository: string;
    release_notes: string;
    releases: string;
    license: string;
    notice: string;
    issues: string;
    user_guide: string;
  };
  updates: UpdateStatus;
};

/** Settings › Remote access (`/api/remote`, desktop only). */
export type RemoteStatus = {
  enabled: boolean;
  running: boolean;
  address: string;
  port: number;
  /** `ip:port` the server listens on while it runs. */
  bound: string | null;
  url: string | null;
  public_url: string;
  /** Bound to something other than loopback (reachable from the network). */
  exposed: boolean;
  allow_terminals: boolean;
  error: string | null;
  addresses: {
    address: string;
    interface: string;
    kind: "loopback" | "tailscale" | "lan";
  }[];
  devices: {
    id: string;
    name: string;
    created_at: number;
    last_seen: number | null;
  }[];
  ntfy: {
    server: string;
    topic: string;
    details: boolean;
    events: {
      approval: boolean;
      finished: boolean;
      failed: boolean;
      limit: boolean;
    };
    token_saved: boolean;
    configured: boolean;
    error: string | null;
  };
};
export type RemotePairing = {
  link: string;
  base: string;
  expires_in: number;
  qr: { size: number; rows: string[] };
};
export type NtfyChange = Partial<
  Pick<RemoteStatus["ntfy"], "server" | "topic" | "details">
> & {
  events?: Partial<RemoteStatus["ntfy"]["events"]>;
  /** A new access token; "" removes the saved one. */
  token?: string;
};

const get = <T>(path: string) => request<T>(path);
const readJobVerification = createVerificationReader((job_ids) =>
  send("/api/jobs/verification-refresh", "POST", { job_ids }),
);

async function send<T>(
  path: string,
  method: string,
  body?: unknown,
): Promise<T> {
  return request<T>(path, method, body);
}

export type ParallelPlan = {
  id: string;
  goal: string;
  source: string;
  lead_note: string;
  verify_status: string;
  workers: {
    item: { id: string; title: string; prompt: string };
    worktree_path: string;
    branch: string;
    status: string;
  }[];
};
export type GuardianStatus = {
  enabled: boolean;
  last_run?: number;
  last_result?: { tests: { hint?: string; executed: boolean } };
};
export type ContextPreview = {
  items: {
    path: string;
    kind: "file" | "dir" | string;
    included: boolean;
    reason: string;
    bytes: number;
    total_bytes: number | null;
    from_line: number | null;
    to_line: number | null;
    entries: string[];
    truncated: boolean;
  }[];
  included_bytes: number;
  estimated_tokens: number;
  truncated: boolean;
};
export const api = {
  parallelPlan: () => get<{ plan: ParallelPlan | null }>("/api/parallel"),
  prepareParallel: (goal: string) =>
    send<{ ok: boolean; error?: string; plan?: ParallelPlan }>(
      "/api/parallel/prepare",
      "POST",
      { goal },
    ),
  parallelWorkerStatus: (worker_id: string, status: string) =>
    send("/api/parallel/worker-status", "POST", { worker_id, status }),
  verifyParallel: () =>
    send<{ ok: boolean; conflicts?: { worker: string; detail: string }[] }>(
      "/api/parallel/verify",
      "POST",
      {},
    ),
  cleanupParallel: () =>
    send<{ cleaned: number }>("/api/parallel/cleanup", "POST", {}),
  guardianStatus: () => get<GuardianStatus>("/api/guardian"),
  startCompare: (body: StartCompareRequest) =>
    send<CompareRecord>("/api/compare", "POST", body),
  compare: (id: string) =>
    get<CompareRecord>(`/api/compare/${encodeURIComponent(id)}`),
  compares: (workspace = "") =>
    get<{ compares: CompareRecord[] }>(
      `/api/compares${workspace ? `?workspace=${encodeURIComponent(workspace)}` : ""}`,
    ),
  keepCompare: (id: string, model: string, acceptUnverified = false) =>
    send<CompareRecord>(`/api/compare/${encodeURIComponent(id)}/keep`, "POST", {
      model,
      accept_unverified: acceptUnverified,
    }),
  recoverCompare: (id: string) =>
    send<CompareRecord>(
      `/api/compare/${encodeURIComponent(id)}/recover`,
      "POST",
      {},
    ),
  discardCompare: (id: string) =>
    send<CompareRecord>(
      `/api/compare/${encodeURIComponent(id)}/discard`,
      "POST",
      {},
    ),
  cancelCompare: (id: string) =>
    send<CompareRecord>(
      `/api/compare/${encodeURIComponent(id)}/cancel`,
      "POST",
      {},
    ),
  worktreeTask: (id: string) =>
    get<WorktreeTask>(`/api/worktree-tasks/${encodeURIComponent(id)}`),
  worktreeTasks: (workspace = "") =>
    get<{ workspace: string; tasks: WorktreeTask[] }>(
      `/api/worktree-tasks${workspace ? `?workspace=${encodeURIComponent(workspace)}` : ""}`,
    ),
  /** apply | keep-branch | discard */
  closeWorktreeTask: (
    id: string,
    action: "apply" | "keep-branch" | "discard",
  ) =>
    send<WorktreeTask>(
      `/api/worktree-tasks/${encodeURIComponent(id)}/${action}`,
      "POST",
      {},
    ),
  compareScoreboard: (workspace = "") =>
    get<{ workspace: string; rows: CompareScore[] }>(
      `/api/compare/scoreboard${workspace ? `?workspace=${encodeURIComponent(workspace)}` : ""}`,
    ),
  runGuardian: () => send("/api/guardian/run", "POST", {}),
  health: () => get<Health>("/api/health"),
  onboarding: () =>
    get<{
      completed: boolean;
      suggested_workspace: string;
    }>("/api/onboarding"),
  completeOnboarding: (body: Record<string, unknown>) =>
    send<{ ok: boolean; workspace: string; session_id: string }>(
      "/api/onboarding",
      "POST",
      body,
    ),
  config: () => get<Record<string, unknown>>("/api/config"),
  saveConfig: (
    values: Record<string, unknown>,
    api_key = "",
    api_key_env = "",
  ) =>
    send<Record<string, unknown>>("/api/config", "PUT", {
      values,
      api_key,
      api_key_env,
    }),
  /** Allowance rows; `refresh` re-checks vendor accounts. */
  allowance: (refresh = false) =>
    get<AllowanceResponse>(`/api/allowance${refresh ? "?refresh=1" : ""}`),
  /** The composer's only source of rows (vendor + local). */
  picker: (refresh = false) =>
    get<PickerResponse>(`/api/picker${refresh ? "?refresh=1" : ""}`),
  /** Local and cached rows only; does not start provider or network probes. */
  pickerCached: () => get<PickerResponse>("/api/picker?cached=1"),
  accounts: (refresh = false) =>
    get<AccountsResponse>(`/api/accounts${refresh ? "?refresh=1" : ""}`),
  accountsCached: () => get<AccountsResponse>("/api/accounts?cached=1"),
  connectAccount: (vendor: string) =>
    send<{
      ok: boolean;
      state: "started" | "unsupported" | "already_running" | string;
      note?: string;
      hint?: string;
      lines?: string[];
    }>(`/api/accounts/${encodeURIComponent(vendor)}/connect`, "POST", {}),
  /** Contract extension: buffered login output for a running Connect. */
  loginProgress: async (vendor: string): Promise<LoginProgress> => {
    const raw = await get<{
      running: boolean;
      cancellation_requested?: boolean;
      lines?: (string | { line?: string })[];
      done: LoginProgress["done"];
    }>(`/api/accounts/${encodeURIComponent(vendor)}/login`);
    // The engine sends {vendor, line, url} records; the page shows text lines.
    return {
      running: raw.running,
      cancellation_requested: raw.cancellation_requested === true,
      done: raw.done,
      lines: (raw.lines || []).map((entry) =>
        typeof entry === "string" ? entry : String(entry.line ?? ""),
      ),
    };
  },
  cancelLogin: (vendor: string) =>
    send<{ ok: boolean }>(
      `/api/accounts/${encodeURIComponent(vendor)}/cancel-login`,
      "POST",
      {},
    ),
  disconnectAccount: (vendor: string) =>
    send<{ ok: boolean; ran?: string[]; note?: string }>(
      `/api/accounts/${encodeURIComponent(vendor)}/disconnect`,
      "POST",
      { confirm: true },
    ),
  refreshAccount: (vendor: string) =>
    send<VendorStatus>(
      `/api/accounts/${encodeURIComponent(vendor)}/refresh`,
      "POST",
      {},
    ),
  /** Antigravity's agent server: install state (poll while `busy`). */
  antigravityInstallStatus: () =>
    get<AgentInstall>("/api/accounts/antigravity/install"),
  /** Starts the download the user confirmed; progress follows in
   * antigravityInstallStatus(). */
  installAntigravity: () =>
    send<AgentInstall & { started: boolean }>(
      "/api/accounts/antigravity/install",
      "POST",
      { confirm: true },
    ),
  /** Removes the copy ShadowCode installed; the sign-in profile stays. */
  removeAntigravity: () =>
    send<AgentInstall>("/api/accounts/antigravity/uninstall", "POST", {}),
  /** OpenRouter (API key, billed per token). */
  openrouterStatus: () => get<OpenRouterStatus>("/api/openrouter"),
  /** Validates the key with OpenRouter and saves it on this computer; the
   * answer never echoes it back. A rejected key is an error. */
  setOpenrouterKey: async (key: string) => {
    const api_key = key.trim();
    // An empty body removes the key; never let a blank field do that.
    if (!api_key) throw new Error("Paste an OpenRouter API key first.");
    return send<OpenRouterStatus>("/api/openrouter/key", "POST", { api_key });
  },
  removeOpenrouterKey: () =>
    send<OpenRouterStatus>("/api/openrouter/key", "POST", { api_key: "" }),
  refreshOpenrouter: () =>
    send<OpenRouterStatus>("/api/openrouter/refresh", "POST", {}),
  localModels: () => get<LocalCatalog>("/api/local-models"),
  addLocalModel: (path: string) =>
    send<{ ok: boolean; local_engine?: LocalCatalog }>(
      "/api/local-models/add",
      "POST",
      { path },
    ),
  removeLocalModel: (path: string) =>
    send<{ ok: boolean; deleted_weights: boolean; detail?: string }>(
      "/api/local-models/remove",
      "POST",
      { path },
    ),
  importOllama: (tag: string) =>
    send<{ ok: boolean; local_engine?: LocalCatalog }>(
      "/api/local-models/import-ollama",
      "POST",
      { tag },
    ),
  loadLocalModel: (id: string) =>
    send<{ ok: boolean; loaded?: LocalCatalog["loaded"] }>(
      "/api/local-models/load",
      "POST",
      { id },
    ),
  unloadLocalModel: () =>
    send<{ ok: boolean }>("/api/local-models/unload", "POST", {}),
  about: () => get<AboutInfo>("/api/about"),
  /** `auto`: run the daily check first when it is due and allowed. */
  updates: (auto = false) =>
    get<UpdateStatus>(`/api/updates${auto ? "?auto=1" : ""}`),
  checkUpdates: () => send<UpdateStatus>("/api/updates/check", "POST", {}),
  dismissUpdate: (version: string) =>
    send<UpdateStatus>("/api/updates/dismiss", "POST", { version }),
  /** The built-in download catalog with this computer's recommendation. */
  modelDownloads: () => get<DownloadCatalog>("/api/local-models/downloads"),
  /** Starts, or resumes, one download (refused offline or without space). */
  startModelDownload: (id: string) =>
    send<DownloadCatalog>("/api/local-models/downloads/start", "POST", { id }),
  pauseModelDownload: (id: string) =>
    send<DownloadCatalog>("/api/local-models/downloads/pause", "POST", { id }),
  /** Stops and deletes the partial file. */
  cancelModelDownload: (id: string) =>
    send<DownloadCatalog>("/api/local-models/downloads/cancel", "POST", { id }),
  /** Deletes a downloaded model (unloading it first). */
  deleteModelDownload: (id: string) =>
    send<DownloadCatalog>("/api/local-models/downloads/delete", "POST", { id }),
  remoteStatus: () => get<RemoteStatus>("/api/remote"),
  saveRemote: (
    values: Partial<
      Pick<
        RemoteStatus,
        "enabled" | "address" | "port" | "public_url" | "allow_terminals"
      >
    >,
  ) => send<RemoteStatus>("/api/remote", "PUT", values),
  pairRemote: (host?: string) =>
    send<RemotePairing>("/api/remote/pair", "POST", { host: host || "" }),
  revokeRemote: (id?: string) =>
    send<RemoteStatus>(
      "/api/remote/devices/revoke",
      "POST",
      id ? { id } : { all: true },
    ),
  saveNtfy: (values: NtfyChange) =>
    send<RemoteStatus>("/api/remote/ntfy", "PUT", values),
  testNtfy: () => send<{ ok: boolean }>("/api/remote/ntfy/test", "POST", {}),
  codeIntelStatus: () => get<CodeIntelStatus>("/api/code-intel/status"),
  saveCodeIntel: (values: Partial<CodeIntelSettings>) =>
    send<{ ok: boolean; config: CodeIntelSettings }>(
      "/api/code-intel/config",
      "POST",
      values,
    ),
  installLanguageServer: (packageId: string) =>
    send<{ ok: boolean; started: boolean }>("/api/code-intel/install", "POST", {
      package: packageId,
    }),
  removeLanguageServer: (packageId: string) =>
    send<{ ok: boolean; removed: boolean }>(
      "/api/code-intel/uninstall",
      "POST",
      { package: packageId },
    ),
  stopLanguageServers: () =>
    send<{ ok: boolean; stopped: number }>(
      "/api/code-intel/servers/stop",
      "POST",
      {},
    ),
  installEmbeddingModel: (model: string) =>
    send<{ ok: boolean; started: boolean }>(
      "/api/code-intel/embeddings/install",
      "POST",
      { model },
    ),
  removeEmbeddingModel: (model: string) =>
    send<{ ok: boolean; removed: boolean }>(
      "/api/code-intel/embeddings/remove",
      "POST",
      { model },
    ),
  reindexCode: () =>
    send<{ ok: boolean; embedding_started: boolean }>(
      "/api/code-intel/reindex",
      "POST",
      {},
    ),
  setSessionTarget: (sessionId: string, targetId: string) =>
    send<{ ok: boolean }>(
      `/api/sessions/${encodeURIComponent(sessionId)}/target`,
      "POST",
      { target_id: targetId },
    ),
  projects: () => get<{ projects: Project[] }>("/api/projects"),
  openProject: (path: string) =>
    send<{
      path: string;
      session_id: string;
      needs_trust?: boolean;
      name?: string;
      permissions?: Record<string, unknown>;
    }>("/api/projects", "POST", { path }),
  trustProject: (path: string) =>
    send<{ ok: boolean; path: string; session_id: string }>(
      "/api/projects/trust",
      "POST",
      { path },
    ),
  sessions: (q = "") =>
    get<{ sessions: Session[] }>(
      `/api/sessions${q ? `?q=${encodeURIComponent(q)}` : ""}`,
    ),
  renameSession: (id: string, title: string) =>
    send<{ ok: boolean }>(`/api/sessions/${id}`, "PATCH", {
      workspace: "",
      title,
    }),
  deleteSession: (id: string) =>
    send<{ ok: boolean }>(`/api/sessions/${id}`, "DELETE"),
  doctor: () => get<DoctorReport>("/api/doctor"),
  sandboxStatus: () => get<SandboxStatus>("/api/sandbox/status"),
  goals: () => get<{ goals: Goal[] }>("/api/goals"),
  createGoal: (instruction: string, run: boolean, session_id?: string) =>
    send<Goal>("/api/goals", "POST", {
      instruction,
      run,
      session_id: session_id || null,
    }),
  runGoal: (id: string, session_id?: string) =>
    send<Goal>(`/api/goals/${id}/run`, "POST", {
      session_id: session_id || null,
    }),
  abandonGoal: (id: string) =>
    send<Goal>(`/api/goals/${id}/abandon`, "POST", {}),
  pauseGoal: (id: string) => send<Goal>(`/api/goals/${id}/pause`, "POST", {}),
  deleteGoal: (id: string) =>
    send<{ ok: boolean }>(`/api/goals/${id}`, "DELETE"),
  setMilestone: (goalId: string, milestoneId: string, status: string) =>
    send<Goal>(`/api/goals/${goalId}/milestones/${milestoneId}`, "POST", {
      status,
      detail: "",
    }),
  background: () => get<{ tasks: BackgroundTask[] }>("/api/background"),
  backgroundTask: (id: string) => get<BackgroundTask>(`/api/background/${id}`),
  startBackground: (name: string, command: string) =>
    send<BackgroundTask>("/api/background", "POST", { name, command }),
  stopBackground: (id: string) =>
    send<BackgroundTask>(`/api/background/${id}/stop`, "POST", {}),
  /** Dev servers of this project (docs/PREVIEW.md). */
  previewServers: () =>
    get<{ workspace: string; servers: PreviewServer[] }>(
      "/api/preview/servers",
    ),
  /** A loopback proxy for `url` that the preview frame loads; `app_origin`
   * is this window's origin (the only one the picker talks to). */
  openPreview: (url: string, app_origin: string) =>
    send<PreviewOpened>("/api/preview/open", "POST", { url, app_origin }),
  hooks: () => get<HookCatalog>("/api/hooks"),
  activateHook: (
    workspace: string,
    path: string,
    hash: string,
    enabled: boolean,
  ) =>
    send<HookCatalog>("/api/hooks/activation", "POST", {
      workspace,
      path,
      hash,
      enabled,
    }),
  mcpServers: () => get<NativeMcpCatalog>("/api/mcp/servers"),
  registerMcp: (definition: Record<string, unknown>, hash = "") =>
    send<NativeMcpCatalog>("/api/mcp/servers", "POST", { definition, hash }),
  activateMcp: (
    workspace: string,
    server: string,
    hash: string,
    enabled: boolean,
  ) =>
    send<NativeMcpCatalog>("/api/mcp/activation", "POST", {
      workspace,
      server,
      hash,
      enabled,
    }),
  removeMcp: (server: string, hash: string) =>
    send<NativeMcpCatalog>("/api/mcp/servers/delete", "POST", { server, hash }),
  worktrees: () =>
    get<{ workspace: string; worktrees: ManagedWorktree[] }>("/api/worktrees"),
  createWorktree: (workspace: string, reference: string) =>
    send<ManagedWorktree>("/api/worktrees", "POST", { workspace, reference }),
  inspectWorktree: (workspace: string, id: string) =>
    send<WorktreeInspection>("/api/worktrees/inspect", "POST", {
      workspace,
      id,
    }),
  reviewWorktreeRepair: (workspace: string, id: string) =>
    send<WorktreeRepairReview>("/api/worktrees/review-repair", "POST", {
      workspace,
      id,
    }),
  repairWorktree: (workspace: string, id: string, hash: string) =>
    send<ManagedWorktree>("/api/worktrees/repair", "POST", {
      workspace,
      id,
      hash,
    }),
  reviewWorktreeCopy: (workspace: string) =>
    send<WorktreeCopyReview>("/api/worktrees/review-changes", "POST", {
      workspace,
    }),
  copyWorktreeChanges: (workspace: string, hash: string) =>
    send<ManagedWorktree>("/api/worktrees/copy-changes", "POST", {
      workspace,
      hash,
    }),
  reviewWorktreeReturn: (workspace: string, id: string) =>
    send<WorktreeReturnReview>("/api/worktrees/review-return", "POST", {
      workspace,
      id,
    }),
  returnWorktreeChanges: (workspace: string, id: string, hash: string) =>
    send<ManagedWorktree>("/api/worktrees/return", "POST", {
      workspace,
      id,
      hash,
    }),
  worktreeRecovery: (workspace: string, id: string) =>
    send<WorktreeRecovery>("/api/worktrees/recovery", "POST", {
      workspace,
      id,
    }),
  restoreWorktree: (workspace: string, id: string, hash: string) =>
    send<ManagedWorktree>("/api/worktrees/restore", "POST", {
      workspace,
      id,
      hash,
    }),
  removeWorktree: (workspace: string, id: string, hash: string) =>
    send<ManagedWorktree>("/api/worktrees/remove", "POST", {
      workspace,
      id,
      hash,
    }),
  plugins: () => get<NativePluginCatalog>("/api/plugins"),
  previewPlugin: (source: { name: string } | { bundle: unknown }) =>
    send<PluginPreview>("/api/plugins/preview", "POST", source),
  installNativePlugin: (workspace: string, preview: PluginPreview) =>
    send<PluginChange>("/api/plugins/install", "POST", {
      workspace,
      bundle: preview.bundle,
      hash: preview.hash,
    }),
  removeNativePlugin: (workspace: string, name: string, hash: string) =>
    send<PluginChange>("/api/plugins/remove", "POST", {
      workspace,
      name,
      hash,
    }),
  taskCheckpoint: (taskId: string) =>
    get<{
      rewindable: boolean;
      checkpoint: { changes: number; paths: string[] } | null;
    }>(`/api/checkpoints/tasks/${taskId}`),
  forkSession: (
    id: string,
    eventId: number,
    title?: string,
    /** Keep only what came before this event's task (Edit & resend). */
    before = false,
  ) =>
    send<{
      fork: { id: string; title: string };
      original: { id: string };
      original_intact: boolean;
      forked_from_event: number;
    }>(`/api/sessions/${id}/fork`, "POST", {
      event_id: eventId,
      title: title || "",
      ...(before ? { before: true } : {}),
    }),
  /** Rewind a finished task's files. `undo_id` undoes the rewind. */
  rewindTask: (taskId: string) =>
    send<{ ok: boolean; restored: string[]; undo_id?: string | null }>(
      `/api/checkpoints/tasks/${taskId}/restore`,
      "POST",
      {},
    ),
  undoRewind: (undoId: string) =>
    send<{ ok: boolean; restored: string[]; task_id: string }>(
      `/api/checkpoints/rewinds/${encodeURIComponent(undoId)}/undo`,
      "POST",
      {},
    ),
  historyPage: (id: string, before: number) =>
    get<HistoryPage>(`/api/sessions/${id}/events?view=window&before=${before}`),
  session: (id: string) =>
    get<SessionDetail>(`/api/sessions/${id}?view=window`),
  activateSession: (id: string) =>
    send<SessionDetail>(`/api/sessions/${id}/activate?view=window`, "POST", {}),
  currentJob: (id: string) =>
    get<{ job: Job | null }>(
      `/api/jobs/current?session_id=${encodeURIComponent(id)}&include_finished=true`,
    ),
  jobs: () => get<{ jobs: Job[] }>("/api/jobs?view=summary&limit=100"),
  createSession: (workspace: string, title = "") =>
    send<{ id: string; workspace: string }>("/api/sessions", "POST", {
      workspace,
      title,
    }),
  files: (path = ".") =>
    get<{
      entries: FileEntry[];
      workspace: string;
      path: string;
      parent: string;
    }>("/api/workspace/files?path=" + encodeURIComponent(path)),
  file: (path: string, full = false) =>
    get<{
      content: string;
      path: string;
      hash: string;
      bytes: number;
      truncated: boolean;
    }>(
      "/api/workspace/file?path=" +
        encodeURIComponent(path) +
        (full ? "&full=true" : ""),
    ),
  fileRevision: (path: string) =>
    get<{ path: string; hash: string; bytes: number }>(
      "/api/workspace/file?path=" + encodeURIComponent(path) + "&head=true",
    ),
  saveFile: (path: string, content: string, expectedHash: string) =>
    send<{ path: string; hash: string; bytes: number }>(
      "/api/workspace/file?path=" + encodeURIComponent(path),
      "PUT",
      { content, expected_hash: expectedHash },
    ),
  editorDrafts: (workspace: string) =>
    get<{ workspace: string; drafts: EditorRecoveryDraft[] }>(
      "/api/workspace/editor-drafts?workspace=" + encodeURIComponent(workspace),
    ),
  saveEditorDraft: (
    workspace: string,
    path: string,
    base: string,
    draft: string,
    baseHash: string,
    expectedRevision: string,
  ) =>
    send<EditorRecoveryDraft>(
      "/api/workspace/editor-draft?path=" + encodeURIComponent(path),
      "PUT",
      {
        workspace,
        base,
        draft,
        base_hash: baseHash,
        expected_revision: expectedRevision,
      },
    ),
  deleteEditorDraft: (
    workspace: string,
    path: string,
    expectedRevision: string,
  ) =>
    send<{ removed: boolean }>(
      "/api/workspace/editor-draft?path=" + encodeURIComponent(path),
      "DELETE",
      { workspace, expected_revision: expectedRevision },
    ),
  git: () =>
    get<{
      status: string;
      log: string;
      diff: string;
      porcelain?: string;
      files?: { path: string; label: string }[];
      repo?: boolean;
    }>("/api/workspace/git"),
  gitDiff: (path = "") =>
    get<{
      diff: string;
      staged: string;
      hunks: DiffHunk[];
      staged_hunks: DiffHunk[];
      untracked: boolean;
      binary: boolean;
      truncated: boolean;
    }>("/api/workspace/diff?path=" + encodeURIComponent(path)),
  gitAdd: (paths: string[]) =>
    send<{ ok: boolean }>("/api/workspace/git/add", "POST", {
      message: "",
      paths,
    }),
  gitCommit: (message: string, paths: string[] = []) =>
    send<{ ok: boolean }>("/api/workspace/git/commit", "POST", {
      message,
      paths,
    }),
  hunkAction: (path: string, hunk: DiffHunk, action: "accept" | "reject") =>
    send<{ ok: boolean; action: string; path: string }>(
      "/api/workspace/diff/hunk",
      "POST",
      { path, hunk, action },
    ),
  exec: (command: string, timeout = 60) =>
    send<ExecResult>("/api/workspace/exec", "POST", { command, timeout }),
  status: () =>
    get<{
      workspace: string;
      model: {
        default: string;
        provider: string;
        endpoint?: string;
        name?: string;
        api_key_env?: string;
        context_limit?: number;
      };
      permissions: { level: string; network?: boolean; mode?: string };
      onboarding?: { completed: boolean };
      routing?: Record<string, string | boolean>;
      trusted?: boolean;
    }>("/api/workspace/status"),
  /** Starts (or queues) a task. A cloud route that needs the user's consent
   * answers `needs_consent` (HTTP 409 or an IPC value); nothing is created
   * until the request is repeated with `handoff_consent: true`. */
  startJob: async (
    body: StartJobRequest,
  ): Promise<{ job: Job } | { consent: ConsentRequest }> => {
    try {
      const value = await send<Job | ConsentRequest>("/api/jobs", "POST", {
        purpose: "coder",
        ...body,
        images: body.images?.length ? body.images : undefined,
      });
      const consent = consentFrom(value);
      return consent ? { consent } : { job: value as Job };
    } catch (error) {
      const consent =
        error instanceof ApiError ? consentFrom(error.body) : null;
      if (consent) return { consent };
      throw error;
    }
  },
  startTestJob: (body: CheckJobRequest) =>
    send<Job>("/api/jobs/test", "POST", body),
  job: (id: string) => get<Job>(`/api/jobs/${id}`),
  jobVerification: readJobVerification,
  cancelJob: (id: string, only_if_queued = false) =>
    send<Job>(`/api/jobs/${id}/cancel`, "POST", { only_if_queued }),
  pauseJob: (id: string) => send<Job>(`/api/jobs/${id}/pause`, "POST", {}),
  resumeJob: (id: string) => send<Job>(`/api/jobs/${id}/resume`, "POST", {}),
  steerJob: (id: string, instruction: string, path?: string) =>
    send<Job>(`/api/jobs/${id}/steer`, "POST", {
      instruction,
      ...(path ? { path } : {}),
    }),
  noteJobEdit: (id: string, path: string, detail = "") =>
    send<Job>(`/api/jobs/${id}/note_edit`, "POST", { path, detail }),
  rewindJob: (id: string) =>
    send<{
      ok: boolean;
      restored: string[];
      note?: string;
    }>(`/api/jobs/${id}/rewind`, "POST", {}),
  /** Pending approvals (for one conversation, or all) and project jobs in
   * one read, plus the broadcast types after which to read it again. */
  feed: (sessionId?: string) =>
    get<{
      approvals: Approval[];
      jobs: Job[];
      events?: string[];
      /** Every conversation with a pending approval. */
      waiting?: string[];
    }>(
      `/api/feed?limit=100${sessionId ? `&session_id=${encodeURIComponent(sessionId)}` : ""}`,
    ),
  /** Added/removed line counts for many files at once; `null` for binary
   * or unknown files. */
  diffStats: (paths: string[]) =>
    send<{ stats: Record<string, { add: number; del: number } | null> }>(
      "/api/workspace/diffstat",
      "POST",
      { paths },
    ),
  approvals: (sessionId?: string) =>
    get<{ approvals: Approval[] }>(
      `/api/approvals${sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : ""}`,
    ),
  decide: (
    id: string,
    decision: "approve" | "deny",
    sessionId?: string,
    options: { scope?: "once" | "task"; note?: string } = {},
  ) =>
    send<Approval>(`/api/approvals/${id}`, "POST", {
      decision,
      session_id: sessionId,
      ...(options.scope ? { scope: options.scope } : {}),
      ...(options.note ? { note: options.note } : {}),
    }),
  /** Files and folders for the composer's @ menu, best matches first. */
  mentions: (query: string, limit = 12) =>
    get<{
      items: { path: string; kind: "file" | "dir" }[];
      truncated: boolean;
    }>(`/api/workspace/mentions?q=${encodeURIComponent(query)}&limit=${limit}`),
  /** Exact byte-bounded ShadowCode @-mention attachment preview. */
  contextPreview: (mentions: { path: string; kind: "file" | "dir" }[]) =>
    send<ContextPreview>("/api/workspace/context-preview", "POST", {
      mentions,
    }),
  /** The files one task changed (per-task review). */
  review: (taskId: string) =>
    get<{
      task_id: string;
      session_id: string;
      workspace: string;
      busy: boolean;
      files: ReviewFile[];
    }>(`/api/review/tasks/${encodeURIComponent(taskId)}`),
  reviewFile: (taskId: string, path: string) =>
    get<ReviewFileDetail>(
      `/api/review/tasks/${encodeURIComponent(taskId)}/file?path=${encodeURIComponent(path)}`,
    ),
  /** Put one hunk (or, without `hunk`, the whole file) back as it was
   * before the task; answers the file's review after the change. */
  reviewUndo: (taskId: string, path: string, hunk?: string) =>
    send<ReviewFileDetail>(
      `/api/review/tasks/${encodeURIComponent(taskId)}/undo`,
      "POST",
      { path, hunk: hunk || "" },
    ),
  instructions: () =>
    get<{ content: string; exists: boolean }>("/api/workspace/instructions"),
  saveInstructions: (content: string) =>
    send<{ ok: boolean }>("/api/workspace/instructions", "PUT", { content }),
  skills: () =>
    get<{ skills: ProjectSkill[]; issues?: string[] }>("/api/workspace/skills"),
  saveSkill: (name: string, content: string, expectedHash?: string) =>
    send<{ ok: boolean }>("/api/workspace/skills", "PUT", {
      name,
      content,
      expected_hash: expectedHash,
    }),
  attach: (filename: string, text: string) =>
    send<{ path: string; kind?: string }>("/api/workspace/attach", "POST", {
      filename,
      text,
    }),
  attachImage: (filename: string, data_base64: string) =>
    send<{ path: string; mime: string; bytes: number; kind: string }>(
      "/api/workspace/attach-image",
      "POST",
      { filename, data_base64 },
    ),
  branchSession: (sessionId: string, title = "") =>
    send<{ id: string; parent_id: string }>(
      `/api/sessions/${sessionId}/branch`,
      "POST",
      { workspace: "", title },
    ),
  agents: () => get<{ agents: AgentInfo[]; issues: string[] }>("/api/agents"),
  commands: () =>
    get<{
      commands: {
        name: string;
        description: string;
        arg_spec: string;
        alias: string;
        source: string;
      }[];
    }>("/api/commands"),
  runCommand: (
    name: string,
    args = "",
    sessionId?: string,
    options: { model?: string; purpose?: string; queue?: boolean } = {},
  ) =>
    send<CommandResult>("/api/commands/run", "POST", {
      name,
      args,
      session_id: sessionId,
      ...options,
    }),
};

export type CommandResult = {
  handled: boolean;
  text: string;
  kind:
    | "text"
    | "card"
    | "list"
    | "diff"
    | "approval"
    | "error"
    | "overlay"
    | "quit";
  icon: string;
  headline: string;
  body: string;
  items: { label: string; value: string }[];
  diff: string;
  path: string;
  approval_action: string;
  approval_reason: string;
  overlay: string;
  quit: boolean;
  passthrough: boolean;
  metadata: Record<string, unknown>;
};

export type PluginFile = {
  path: string;
  kind: string;
  hash: string;
  content?: string;
  status?: "unchanged" | "modified" | "missing" | "unreadable";
  error?: string | null;
};
export type PluginEntry = {
  name: string;
  version: string;
  description: string;
  hash: string;
  state?: "prepared" | "installed" | "removing";
  files: PluginFile[];
};
export type NativePluginCatalog = {
  format: "native-plugins-v1";
  workspace: string;
  trusted: boolean;
  read_only: boolean;
  installed: PluginEntry[];
  available: PluginEntry[];
  issues: string[];
  legacy: string[];
};
export type PluginPreview = {
  bundle: { name: string; version: string; description: string };
  hash: string;
  files: PluginFile[];
};
export type PluginChange = {
  catalog: NativePluginCatalog;
  result: { name: string; retained?: { path: string; reason: string }[] };
};
