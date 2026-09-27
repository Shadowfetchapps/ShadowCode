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
