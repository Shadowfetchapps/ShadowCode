import assert from "node:assert/strict";

export const initializeMarker = "__SHADOW_CLEAN_MCP_INITIALIZE__";
export const toolsMarker = "__SHADOW_CLEAN_MCP_TOOLS__";

const expectedTools = [
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
];
const object = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);

function response(output, marker, id) {
  const lines = output.split("\n").filter((line) => line.startsWith(marker));
  assert.equal(lines.length, 1, `Expected one ${marker} receipt`);
  const reply = JSON.parse(lines[0].slice(marker.length));
  assert.ok(object(reply), "MCP response must be an object");
  assert.equal(reply.jsonrpc, "2.0");
  assert.equal(reply.id, id);
  assert.ok(!Object.hasOwn(reply, "error"), "MCP response contains an error");
  assert.ok(object(reply.result), "MCP response must contain a result object");
  return reply.result;
}

export function validateCleanHostMcp(output) {
  const info = response(output, initializeMarker, 1);
  assert.equal(
    info.protocolVersion,
    "2025-11-25",
    "Unsupported MCP protocol negotiation",
  );
  assert.equal(info.serverInfo?.name, "ShadowCode");
  assert.ok(object(info.capabilities?.tools), "Server must advertise tools");
  const catalog = response(output, toolsMarker, 2);
  assert.ok(Array.isArray(catalog.tools), "Missing tools catalog");
  const names = new Set();
  for (const tool of catalog.tools) {
    assert.ok(
      object(tool) && typeof tool.name === "string",
      "Invalid tool definition",
    );
    assert.ok(!names.has(tool.name), `Duplicate MCP tool ${tool.name}`);
    names.add(tool.name);
    assert.equal(
      tool.inputSchema?.type,
      "object",
      `Invalid schema for ${tool.name}`,
    );
    assert.ok(
      object(tool.inputSchema.properties),
      `Missing properties for ${tool.name}`,
    );
    assert.ok(
      Array.isArray(tool.inputSchema.required),
      `Missing required list for ${tool.name}`,
    );
    assert.ok(
      tool.inputSchema.required.every(
        (key) =>
          typeof key === "string" &&
          Object.hasOwn(tool.inputSchema.properties, key),
      ),
      `Invalid required properties for ${tool.name}`,
    );
  }
  for (const name of expectedTools)
    assert.ok(names.has(name), `Missing MCP tool ${name}`);
  for (const [name, key] of [
    ["shadow_run", "task"],
    ["shadow_sqlite", "path"],
  ]) {
    const schema = catalog.tools.find((tool) => tool.name === name).inputSchema;
    assert.equal(schema.properties[key]?.type, "string");
    assert.ok(schema.required.includes(key), `${name} must require ${key}`);
  }
  return { protocolVersion: info.protocolVersion, tools: [...names] };
}
