import { afterEach, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import type { LocalRuntimeReceipt } from "../api";
import { LocalModelDetails } from "./LocalModelDetails";
import { TaskSummary } from "./TaskSummary";
import { replay } from "../lib/transcript";
import { parseLocalRuntimeReceipt } from "../lib/provenance";

afterEach(cleanup);
const receipt: LocalRuntimeReceipt = {
  model_id: "local:gguf:model-a",
  preparation_seconds: 1,
  automatic_cpu_fallback_allowed: false,
  runtime: {
    backend: "vulkan",
    context_tokens: 8192,
    cpu_fallback: false,
    provenance: {
      schema: 1,
      identity_kind: "filesystem_metadata",
      files: {
        model: {
          path: "/models/weights.gguf",
          bytes: 100000,
          device: "2049",
          inode: "18446744073709551600",
          modified_ns: "1720000000123456789",
        },
        runtime: { path: "/runtime/llama-server", bytes: 10000 },
      },
      model: {
        gguf_version: 3,
        architecture: "qwen3",
        header_sha256: "a".repeat(64),
        header_bytes: 512,
        quantization: {
          file_type: 18,
          version: 2,
          tensor_type_counts: { 0: 2, 14: 40 },
        },
        chat_template: { sha256: "b".repeat(64), bytes: 70000 },
      },
      runtime: {
        reported_version: "fixture-version",
        reported_commit: "fixture-commit",
        reported_generation_defaults: {
          temperature: 0.8,
          top_k: 40,
          seed: 4294967295,
        },
        reported_chat_template: { sha256: "c".repeat(64), bytes: 70000 },
      },
      context: { requested_tokens: 8192, reported_tokens: 8192 },
      gpu: {
        requested_mode: "all",
        launch_mode: "off",
        reported_backend: "vulkan",
      },
    },
  },
  request_policy: {
    sampling_source: "runtime_defaults",
    sampling_overrides: {},
    max_tokens_policy: "context_budget_per_request",
    chat_template_kwargs: { enable_thinking: false },
  },
};

it("shows recorded identities and settings with their actual limits", () => {
  render(<LocalModelDetails receipt={receipt} />);
  expect(screen.getByText("Local model details")).toBeTruthy();
  expect(screen.getByText("/models/weights.gguf")).toBeTruthy();
  expect(screen.getByText("2049 / 18446744073709551600")).toBeTruthy();
  expect(screen.getByText("fixture-version")).toBeTruthy();
  expect(screen.getByText("off")).toBeTruthy();
  expect(screen.getByText("vulkan")).toBeTruthy();
  expect(screen.getByText("b".repeat(64))).toBeTruthy();
  expect(screen.getByText("Sampling default · temperature")).toBeTruthy();
  expect(screen.getByText("Template thinking override")).toBeTruthy();
  expect(screen.getByText(/not full weight hashes/)).toBeTruthy();
  expect(screen.getByText(/do not prove GPU offload/)).toBeTruthy();
});

it("does not manufacture provenance for old, unknown or malformed receipts", () => {
  const { container, rerender } = render(<LocalModelDetails />);
  expect(container.textContent).toBe("");
  rerender(
    <LocalModelDetails
      receipt={{ model_id: "old", runtime: { backend: "cpu" } }}
    />,
  );
  expect(container.textContent).toBe("");
  expect(
    parseLocalRuntimeReceipt({
      ...receipt,
      runtime: { provenance: { schema: 2 } },
    }),
  ).toBeUndefined();
  const incomplete = {
    ...receipt,
    runtime: {
      ...receipt.runtime,
      provenance: {
        ...receipt.runtime!.provenance!,
        model: { architecture: { unexpected: true } },
        runtime: null,
        context: [],
        gpu: null,
      },
    },
  };
  rerender(<LocalModelDetails receipt={incomplete} />);
  expect(screen.getByText(/Sampling defaults were not reported/)).toBeTruthy();
  expect(screen.queryByText("Architecture")).toBeNull();
});

it("confirms a compatibility template only against its recorded runtime identity", () => {
  const applied = structuredClone(receipt);
  applied.runtime!.provenance!.template_override = {
    profile: "hermes-2-pro-llama-3-8b-tool-use-v1",
    source: "bundled_llama_cpp_template",
    source_commit: "d".repeat(40),
    template: { sha256: "c".repeat(64), bytes: 70000 },
    runtime_template_verified: true,
  };
  const { rerender } = render(<LocalModelDetails receipt={applied} />);
  expect(screen.getByText("Bundled Hermes 2 Pro template")).toBeTruthy();
  expect(screen.getByText("Matched runtime-reported template")).toBeTruthy();
  expect(screen.getByText("b".repeat(64))).toBeTruthy();
  applied.runtime!.provenance!.runtime.reported_chat_template!.sha256 =
    "e".repeat(64);
  rerender(<LocalModelDetails receipt={applied} />);
  expect(screen.getByText("Not confirmed")).toBeTruthy();
  expect(screen.queryByText("Matched runtime-reported template")).toBeNull();
  delete applied.runtime!.provenance!.template_override;
  rerender(<LocalModelDetails receipt={applied} />);
  expect(screen.queryByText("Tool compatibility")).toBeNull();
});

it("replays provenance only for its task and preserves the historical receipt", () => {
  const state = replay([
    {
      id: 1,
      ts: 10,
      task_id: "first",
      type: "local.runtime_ready",
      payload: receipt,
    },
    {
      id: 2,
      ts: 11,
      task_id: "first",
      type: "agent.completed",
      payload: { success: true, summary: "Done" },
    },
    {
      id: 3,
      ts: 12,
      task_id: "next",
      type: "local.runtime_progress",
      payload: { phase: "loading" },
    },
    {
      id: 4,
      ts: 13,
      task_id: "first",
      type: "local.runtime_ready",
      payload: { ...receipt, model_id: "must-not-replace-finished" },
    },
  ]);
  expect(state.activeTaskId).toBe("next");
  expect(state.activity.first.localRuntime?.model_id).toBe(receipt.model_id);
  expect(state.activity.next.localRuntime).toBeUndefined();
  render(<TaskSummary activity={state.activity.first} onReview={() => {}} />);
  expect(screen.getByText("Local model details")).toBeTruthy();
  expect(screen.getByText("local:gguf:model-a")).toBeTruthy();
});

it("keeps background receipts separate without replacing the active task", () => {
  const state = replay([
    {
      id: 1,
      ts: 10,
      task_id: "main",
      type: "agent.started",
      payload: { task: "Main task" },
    },
    {
      id: 2,
      ts: 11,
      task_id: "background",
      type: "local.runtime_ready",
      payload: receipt,
    },
  ]);
  expect(state.activeTaskId).toBe("main");
  expect(state.activity.main.localRuntime).toBeUndefined();
  expect(state.activity.background.localRuntime?.model_id).toBe(
    receipt.model_id,
  );
});

function withCapabilities(capabilities: unknown) {
  const copy = structuredClone(receipt);
  Object.assign(copy.runtime!.provenance!.runtime, {
    reported_tool_capabilities: capabilities,
  });
  return copy;
}

function detail(label: string) {
  return screen.getByText(label, { selector: "dt" }).nextElementSibling
    ?.textContent;
}

it("distinguishes runtime template reports from coding qualification", () => {
  render(
    <LocalModelDetails
      receipt={withCapabilities({
        field_status: "reported",
        supports_tools: false,
        supports_tool_calls: true,
        supports_parallel_tool_calls: false,
        supports_object_arguments: true,
        invalid_fields: [],
      })}
    />,
  );
  expect(detail("Tool definitions · runtime report")).toBe("No");
  expect(detail("Tool-call history · runtime report")).toBe("Yes");
  expect(detail("Parallel tool calls · runtime report")).toBe("No");
  expect(detail("Object tool arguments · runtime report")).toBe("Yes");
  expect(
    screen.getByText(
      /do not establish successful tool execution or coding quality/,
    ),
  ).toBeTruthy();
  expect(
    screen.queryByText(/Tools verified|Coding verified|Tool support verified/),
  ).toBeNull();
});

it("keeps missing and malformed capability declarations unknown", () => {
  const { rerender } = render(
    <LocalModelDetails
      receipt={withCapabilities({
        field_status: "missing",
        supports_tools: true,
        supports_tool_calls: true,
      })}
    />,
  );
  expect(detail("Tool definitions · runtime report")).toBe("Not reported");
  expect(detail("Tool-call history · runtime report")).toBe("Not reported");
  expect(detail("Runtime tool capability fields")).toBe("Not reported");
  rerender(
    <LocalModelDetails
      receipt={withCapabilities({
        field_status: "reported",
        supports_tools: "true",
        supports_tool_calls: 1,
        supports_parallel_tool_calls: null,
        supports_object_arguments: false,
        invalid_fields: [
          "supports_tools",
          "supports_tool_calls",
          "fake-private-field",
        ],
        private_value: "fake-private-secret",
      })}
    />,
  );
  expect(detail("Tool definitions · runtime report")).toBe("Not reported");
  expect(detail("Tool-call history · runtime report")).toBe("Not reported");
  expect(detail("Object tool arguments · runtime report")).toBe("No");
  expect(detail("Invalid capability fields")).toBe(
    "supports_tools, supports_tool_calls",
  );
  expect(document.body.textContent).not.toContain("fake-private");
  rerender(
    <LocalModelDetails
      receipt={withCapabilities({
        field_status: "invalid",
        supports_tools: true,
      })}
    />,
  );
  expect(detail("Runtime tool capability fields")).toBe("Invalid report");
  expect(detail("Tool definitions · runtime report")).toBe("Not reported");
});

it("does not add a tool capability claim to historical or unrecognized receipts", () => {
  const { rerender } = render(<LocalModelDetails receipt={receipt} />);
  expect(screen.queryByText("Tool definitions · runtime report")).toBeNull();
  for (const malformed of [
    null,
    [],
    true,
    { field_status: "future", supports_tools: true },
  ]) {
    rerender(<LocalModelDetails receipt={withCapabilities(malformed)} />);
    expect(screen.queryByText("Tool definitions · runtime report")).toBeNull();
  }
});

it("keeps the full named tool-use template identity separate from the default", () => {
  const copy = structuredClone(receipt);
  Object.assign(copy.runtime!.provenance!.runtime, {
    reported_chat_template_tool_use: { sha256: "d".repeat(64), bytes: 90000 },
  });
  render(<LocalModelDetails receipt={copy} />);
  expect(detail("Runtime template SHA-256 · reported")).toBe("c".repeat(64));
  expect(detail("Runtime tool-use template SHA-256 · reported")).toBe(
    "d".repeat(64),
  );
  expect(detail("Runtime tool-use template bytes hashed")).toBe(
    (90000).toLocaleString(),
  );
  expect(screen.queryByText("Matched runtime-reported template")).toBeNull();
});

it("renders only the selected task's runtime tool report after replay", () => {
  const first = withCapabilities({
    field_status: "reported",
    supports_tools: false,
    supports_tool_calls: false,
  });
  const other = withCapabilities({
    field_status: "reported",
    supports_tools: true,
    supports_tool_calls: true,
  });
  other.model_id = "local:gguf:other";
  const state = replay([
    {
      id: 1,
      ts: 10,
      task_id: "first",
      type: "local.runtime_ready",
      payload: first,
    },
    {
      id: 2,
      ts: 11,
      task_id: "other",
      type: "local.runtime_ready",
      payload: other,
    },
  ]);
  render(<TaskSummary activity={state.activity.first} onReview={() => {}} />);
  expect(detail("Tool-call history · runtime report")).toBe("No");
  expect(screen.queryByText("local:gguf:other")).toBeNull();
});
