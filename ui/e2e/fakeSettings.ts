/**
 * Fake Settings routes for the Playwright suite: Code intelligence, project
 * instructions, lifecycle hooks, MCP servers, plugins and Doctor checks.
 * Install it after `installFakeBackend`: it wraps that bridge, answers these
 * reads with realistic content and passes everything else through, so every
 * Settings page can be rendered, checked and captured.
 *
 * Self-contained like fakeBackend.ts (Playwright serialises it).
 */
export function installFakeSettings() {
  const inner = (window as any).__SHADOW_TEST_TRANSPORT__;
  if (!inner) throw new Error("installFakeBackend must run first");
  const fake = (window as any).__SHADOW_FAKE__;
  const log: { method: string; path: string; body: any }[] = fake.log;
  const workspace = "/work/demo";
  const codeIntel = {
    config: {
      lsp: true,
      diagnostics_on_edit: true,
      diagnostics_wait_ms: 3000,
      lsp_idle_minutes: 10,
      max_servers: 4,
      repo_map_tokens: 1024,
      semantic_search: true,
      embedding_model: "",
      servers: {},
    },
    offline: false,
    languages: [
      {
        language: "typescript",
        label: "TypeScript and JavaScript",
        available: true,
        enabled: true,
        server: "typescript-language-server",
        source: "path",
      },
      {
        language: "python",
        label: "Python",
        available: false,
        enabled: true,
        managed_package: "python",
        note: "No Python language server found.",
      },
    ],
    servers: [],
    managed: [
      {
        id: "python",
        label: "Python (Pyright)",
        packages: ["pyright@1.1.414"],
        approx_bytes: 19_457_120,
        installed: false,
        installed_bytes: null,
        progress: null,
      },
    ],
    npm: { available: true, path: "/usr/bin/npm" },
    index: {
      files: 12,
      symbols: 80,
      chunks: 30,
      languages: { typescript: 12 },
    },
    embeddings: {
      models: [
        {
          id: "bge-small-en-v1.5-q8",
          name: "BGE small (English) v1.5, Q8_0",
          summary: "Smallest and fastest.",
          bytes: 36_806_944,
          license: "MIT",
          installed: false,
          active: false,
          progress: null,
        },
      ],
      active: null,
      runtime: "/usr/lib/shadowcode/llama-server",
      coverage: null,
      backfill: null,
    },
  };
  const mcp = {
    format: "native-mcp-v1",
    servers: [
      {
        id: "docs",
        name: "docs",
        hash: "h1",
        description: "Search the project documentation",
        timeout_sec: 30,
        env_names: [],
        env_refs: {},
        enabled: false,
        transport: "stdio",
        command: ["npx", "-y", "docs-mcp"],
      },
    ],
    approved: [],
    issues: [],
    workspace,
    trusted: true,
    dirs: [".shadowcode/mcp"],
  };
  const plugins = {
    format: "native-plugins-v1",
    workspace,
    trusted: true,
    read_only: false,
    installed: [
      {
        name: "lint-on-save",
        version: "1.0.0",
        description: "Runs the linter after each edit.",
        hash: "p1",
        state: "installed",
        files: [
          {
            path: ".shadowcode/hooks/lint.yaml",
            kind: "hook",
            hash: "f1",
            status: "unchanged",
          },
        ],
      },
    ],
    available: [],
    issues: [],
    legacy: [],
  };
  const hooks = {
    format: "command-v1",
    workspace,
    trusted: true,
    dirs: [".shadowcode/hooks"],
    hooks: [
      {
        name: "lint",
        events: ["after_edit"],
        builtin: false,
        command: "npm run lint",
        description: "Lint after edits",
        path: ".shadowcode/hooks/lint.yaml",
        hash: "k1",
        enabled: false,
        timeout_sec: 60,
      },
    ],
    approved: [],
  };
  const doctor = {
    ok: true,
    version: "0.33.0-test",
    checks: [
      {
        id: "git",
        ok: true,
        status: "pass",
        label: "Git repository",
        detail: "The project is a Git repository.",
      },
      {
        id: "tests",
        ok: true,
        status: "warn",
        label: "Test command",
        detail: "No test command found.",
        fix: "Add a test script to package.json.",
      },
    ],
    suggestions: [],
  };
  let instructions = "Use TypeScript strict mode.\n";
  function route(method: string, path: string, body: any): unknown {
    if (path === "/api/code-intel/status" && method === "GET") return codeIntel;
    if (path === "/api/mcp/servers" && method === "GET") return mcp;
    if (path === "/api/plugins" && method === "GET") return plugins;
    if (path === "/api/hooks" && method === "GET") return hooks;
    if (path === "/api/doctor" && method === "GET") return doctor;
    if (path === "/api/workspace/instructions" && method === "GET")
      return { content: instructions, exists: true };
    if (path === "/api/workspace/instructions" && method === "PUT") {
      instructions = String(body?.content ?? "");
      return { ok: true };
    }
    return undefined;
  }
  const bridge = {
    ...inner,
    async request(path: string, method: string, body: unknown) {
      const answer = route(method, path, body);
      if (answer === undefined) return inner.request(path, method, body);
      log.push({ method, path, body });
      return JSON.parse(JSON.stringify(answer));
    },
  };
  (window as any).__SHADOW_TEST_TRANSPORT__ = bridge;
}
