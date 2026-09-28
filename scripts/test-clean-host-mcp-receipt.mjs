import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import {
  initializeMarker,
  toolsMarker,
  validateCleanHostMcp,
} from "./clean-host-mcp-receipt.mjs";

function replies() {
  return [
    {
      jsonrpc: "2.0",
      id: 1,
      result: {
        protocolVersion: "2025-11-25",
        serverInfo: { name: "ShadowCode" },
        capabilities: { tools: {} },
      },
    },
    {
      jsonrpc: "2.0",
      id: 2,
      result: {
        tools: [
          "shadow_understand",
          "shadow_doctor",
          "shadow_why",
          "shadow_test",
          "shadow_status",
          "shadow_sqlite",
          "shadow_models",
          "shadow_sessions",
          "shadow_review",
          "shadow_memory",
          "shadow_goal",
          "shadow_run",
          "shadow_jobs",
          "shadow_approve",
          "shadow_checkpoint",
          "shadow_rollback",
          "shadow_tools",
        ].map((name) => {
          const required =
            name === "shadow_run"
              ? ["task"]
              : name === "shadow_sqlite"
                ? ["path"]
                : [];
          return {
            name,
            inputSchema: {
              type: "object",
              properties: Object.fromEntries(
                required.map((key) => [key, { type: "string" }]),
              ),
              required,
            },
          };
        }),
      },
    },
  ];
}
function receipt([initialized, listed]) {
  return `${initializeMarker}${JSON.stringify(initialized)}\n${toolsMarker}${JSON.stringify(listed)}\n`;
}

test("accepts a negotiated MCP handshake and all expected tool schemas", () => {
  assert.equal(validateCleanHostMcp(receipt(replies())).tools.length, 17);
});

for (const [label, mutate] of [
  [
    "tools/list error containing the old grep token",
    (r) => {
      r[1] = {
        jsonrpc: "2.0",
        id: 2,
        error: { code: -32603, message: "tools" },
      };
    },
  ],
  [
    "initialize error containing the server name",
    (r) => {
      r[0] = {
        jsonrpc: "2.0",
        id: 1,
        error: { code: -32603, data: { name: "ShadowCode" } },
      };
    },
  ],
  [
    "error alongside result",
    (r) => {
      r[1].error = null;
    },
  ],
  [
    "wrong response ID",
    (r) => {
      r[1].id = 1;
    },
  ],
  [
    "missing JSON-RPC version",
    (r) => {
      delete r[0].jsonrpc;
    },
  ],
  [
    "unsupported negotiated version",
    (r) => {
      r[0].result.protocolVersion = "1900-01-01";
    },
  ],
  [
    "wrong server",
    (r) => {
      r[0].result.serverInfo.name = "Other";
    },
  ],
  [
    "missing advertised capability",
    (r) => {
      delete r[0].result.capabilities.tools;
    },
  ],
  [
    "empty catalog",
    (r) => {
      r[1].result.tools = [];
    },
  ],
  [
    "missing expected tool",
    (r) => {
      r[1].result.tools.pop();
    },
  ],
  [
    "duplicate tool",
    (r) => {
      r[1].result.tools.push(r[1].result.tools[0]);
    },
  ],
  [
    "missing schema",
    (r) => {
      delete r[1].result.tools[0].inputSchema;
    },
  ],
  [
    "invalid required property",
    (r) => {
      r[1].result.tools[0].inputSchema.required = ["absent"];
    },
  ],
  [
    "missing task argument",
    (r) => {
      r[1].result.tools.find((t) => t.name === "shadow_run").inputSchema = {
        type: "object",
        properties: {},
        required: [],
      };
    },
  ],
]) {
  test(`rejects ${label}`, () => {
    const r = replies();
    mutate(r);
    assert.throws(() => validateCleanHostMcp(receipt(r)));
  });
}
test("rejects malformed, missing and duplicate receipt lines", () => {
  assert.throws(() =>
    validateCleanHostMcp(`${initializeMarker}{not json\n${toolsMarker}{}\n`),
  );
  assert.throws(() => validateCleanHostMcp("__SHADOW_CLEAN_MCP_STDIO__\n"));
  assert.throws(() =>
    validateCleanHostMcp(receipt(replies()) + receipt(replies())),
  );
});

test("shell collects sequential replies without a sleep; host rejects a protocol error", () => {
  const scratch = mkdtempSync(path.join(tmpdir(), "shadow-mcp-receipt-"));
  const helper = fileURLToPath(
    new URL("./clean-host-mcp-smoke.sh", import.meta.url),
  );
  try {
    for (const invalid of [false, true]) {
      const r = replies();
      if (invalid)
        r[1] = {
          jsonrpc: "2.0",
          id: 2,
          error: { code: -32603, message: "tools" },
        };
      const fake = path.join(scratch, "fake-server");
      writeFileSync(
        fake,
        `#!/bin/sh\nwhile IFS= read -r line; do\n case "$line" in\n *'"id":1'*) printf '%s\\n' '${JSON.stringify(r[0])}';;\n *'"id":2'*) printf '%s\\n' '${JSON.stringify(r[1])}';;\n esac\ndone\n`,
        { mode: 0o755 },
      );
      const result = spawnSync(
        "sh",
        [
          helper,
          fake,
          path.join(scratch, "profile"),
          path.join(scratch, "workspace"),
        ],
        { encoding: "utf8", timeout: 10000 },
      );
      assert.ifError(result.error);
      assert.equal(result.status, 0, result.stderr);
      if (invalid) assert.throws(() => validateCleanHostMcp(result.stdout));
      else assert.equal(validateCleanHostMcp(result.stdout).tools.length, 17);
    }
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
});

test("shell terminates a stalled server that ignores SIGTERM", () => {
  const scratch = mkdtempSync(path.join(tmpdir(), "shadow-mcp-timeout-"));
  const helper = fileURLToPath(
    new URL("./clean-host-mcp-smoke.sh", import.meta.url),
  );
  try {
    const fake = path.join(scratch, "fake-server");
    writeFileSync(
      fake,
      "#!/bin/sh\ntrap '' TERM\nwhile IFS= read -r line; do :; done\n",
      { mode: 0o755 },
    );
    // Preserve the production timeout options, shortening only their durations
    // so the uncooperative-server regression does not take 22 seconds in CI.
    writeFileSync(
      path.join(scratch, "timeout"),
      '#!/bin/sh\n[ "$1" = "--kill-after=2s" ] && [ "$2" = "20s" ] || exit 99\nshift 2\nexec /usr/bin/timeout --kill-after=0.1s 0.2s "$@"\n',
      { mode: 0o755 },
    );
    const result = spawnSync(
      "sh",
      [
        helper,
        fake,
        path.join(scratch, "profile"),
        path.join(scratch, "workspace"),
      ],
      {
        encoding: "utf8",
        timeout: 5000,
        env: { ...process.env, PATH: `${scratch}:${process.env.PATH}` },
      },
    );
    assert.ifError(result.error);
    assert.notEqual(result.status, 0);
    assert.ok(!result.stdout.includes(toolsMarker));
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
});
