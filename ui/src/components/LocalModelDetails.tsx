import type { ReactNode } from "react";
import {
  parseLocalRuntimeReceipt,
  provenanceObject as obj,
} from "../lib/provenance";
import "./TaskTimingDetails.css";
import "./LocalModelDetails.css";

const text = (value: unknown) =>
  typeof value === "string" && value.length ? value : undefined;
const number = (value: unknown) =>
  typeof value === "number" && Number.isFinite(value) && value >= 0
    ? value
    : undefined;
const hash = (value: unknown) =>
  typeof value === "string" && /^[a-f0-9]{64}$/.test(value) ? value : undefined;

export function LocalModelDetails({ receipt }: { receipt?: unknown }) {
  const parsed = parseLocalRuntimeReceipt(receipt);
  if (!parsed) return null;
  const p = parsed.runtime!.provenance!;
  const model = obj(p.model);
  const runtime = obj(p.runtime);
  const context = obj(p.context);
  const gpu = obj(p.gpu);
  const quantization = obj(model.quantization);
  const template = obj(model.chat_template);
  const runtimeTemplate = obj(runtime.reported_chat_template);
  const runtimeToolTemplate = obj(runtime.reported_chat_template_tool_use);
  const toolCaps = obj(runtime.reported_tool_capabilities);
  const toolCapsStatus = toolCaps.field_status;
  const hasToolReport =
    toolCapsStatus === "missing" ||
    toolCapsStatus === "invalid" ||
    toolCapsStatus === "reported";
  const override = obj(p.template_override);
  const appliedTemplate = obj(override.template);
  const policy = obj(parsed.request_policy);
  const thinking = obj(policy.chat_template_kwargs).enable_thinking;
  const rows: [string, ReactNode][] = [];
  const add = (label: string, value: ReactNode) => {
    if (value !== undefined && value !== null && value !== "")
      rows.push([label, value]);
  };
  add("Model ID", text(parsed.model_id));
  add("Architecture", text(model.architecture));
  add("GGUF version", number(model.gguf_version));
  add("Quantization · GGUF file type", number(quantization.file_type));
  add("Quantization format version", number(quantization.version));
  const tensorTypes = Object.entries(obj(quantization.tensor_type_counts))
    .filter(
      ([type, count]) => /^\d+$/.test(type) && number(count) !== undefined,
    )
    .map(([type, count]) => `${type}: ${count}`);
  if (tensorTypes.length)
    add("Tensor counts · GGML type code", tensorTypes.join(" · "));
  add("Runtime version · reported", text(runtime.reported_version));
  add("Runtime commit · reported", text(runtime.reported_commit));
  add(
    "Context requested · tokens",
    number(context.requested_tokens)?.toLocaleString(),
  );
  add(
    "Context reported · tokens",
    number(context.reported_tokens)?.toLocaleString(),
  );
  add("GPU mode requested", text(gpu.requested_mode));
  add("GPU mode launched", text(gpu.launch_mode));
  add("Runtime backend · reported", text(gpu.reported_backend));
  if (typeof parsed.runtime!.cpu_fallback === "boolean")
    add(
      "Automatic CPU fallback used",
      parsed.runtime!.cpu_fallback ? "Yes" : "No",
    );
  for (const [label, source] of [
    ["Model", p.files.model],
    ["Projector", p.files.projector],
    ["Runtime", p.files.runtime],
  ] as const) {
    const file = obj(source);
    if (!text(file.path)) continue;
    add(`${label} file`, <code>{text(file.path)}</code>);
    add(`${label} bytes`, number(file.bytes)?.toLocaleString());
    add(`${label} modified · Unix nanoseconds`, text(file.modified_ns));
    if (text(file.device) !== undefined && text(file.inode) !== undefined)
      add(`${label} device / inode`, `${file.device} / ${file.inode}`);
    if (
      number(file.changed_seconds) !== undefined &&
      number(file.changed_nanoseconds) !== undefined
    )
      add(
        `${label} change time · Unix seconds`,
        `${file.changed_seconds}.${String(file.changed_nanoseconds).padStart(9, "0")}`,
      );
  }
  add("GGUF header SHA-256", hash(model.header_sha256));
  add("GGUF header bytes hashed", number(model.header_bytes)?.toLocaleString());
  add("Embedded template SHA-256", hash(template.sha256));
  add(
    "Embedded template bytes hashed",
    number(template.bytes)?.toLocaleString(),
  );
  add("Runtime template SHA-256 · reported", hash(runtimeTemplate.sha256));
  add(
    "Runtime template bytes hashed",
    number(runtimeTemplate.bytes)?.toLocaleString(),
  );
  add(
    "Runtime tool-use template SHA-256 · reported",
    hash(runtimeToolTemplate.sha256),
  );
  add(
    "Runtime tool-use template bytes hashed",
    number(runtimeToolTemplate.bytes)?.toLocaleString(),
  );
  if (hasToolReport) {
    add(
      "Runtime tool capability fields",
      toolCapsStatus === "reported"
        ? "Reported"
        : toolCapsStatus === "invalid"
          ? "Invalid report"
          : "Not reported",
    );
    const fields = [
      ["supports_tools", "Tool definitions · runtime report"],
      ["supports_tool_calls", "Tool-call history · runtime report"],
      ["supports_parallel_tool_calls", "Parallel tool calls · runtime report"],
      ["supports_object_arguments", "Object tool arguments · runtime report"],
    ] as const;
    for (const [key, label] of fields) {
      const value = toolCapsStatus === "reported" ? toolCaps[key] : undefined;
      add(
        label,
        typeof value === "boolean" ? (value ? "Yes" : "No") : "Not reported",
      );
    }
    const invalidFields = toolCaps.invalid_fields;
    const invalid =
      toolCapsStatus === "reported" && Array.isArray(invalidFields)
        ? fields
            .map(([key]) => key)
            .filter((key) => invalidFields.includes(key))
        : [];
    if (invalid.length) add("Invalid capability fields", invalid.join(", "));
  }
  if (
    override.profile === "hermes-2-pro-llama-3-8b-tool-use-v1" &&
    override.source === "bundled_llama_cpp_template"
  ) {
    add("Tool compatibility", "Bundled Hermes 2 Pro template");
    const confirmed =
      override.runtime_template_verified === true &&
      hash(appliedTemplate.sha256) !== undefined &&
      appliedTemplate.sha256 === runtimeTemplate.sha256 &&
      number(appliedTemplate.bytes) !== undefined &&
      appliedTemplate.bytes === runtimeTemplate.bytes;
    add(
      "Template confirmation",
      confirmed ? "Matched runtime-reported template" : "Not confirmed",
    );
    add("Bundled template SHA-256", hash(appliedTemplate.sha256));
    if (
      typeof override.source_commit === "string" &&
      /^[a-f0-9]{40}$/.test(override.source_commit)
    )
      add("Template source commit", override.source_commit);
  }
  const sampling = obj(runtime.reported_generation_defaults);
  for (const [name, value] of Object.entries(sampling)) {
    if (typeof value === "number" && Number.isFinite(value))
      add(`Sampling default · ${name}`, value);
    else if (
      name === "samplers" &&
      Array.isArray(value) &&
      value.every((v) => typeof v === "string")
    )
      add("Sampling order · reported", value.join(", "));
  }
  if (policy.sampling_source === "runtime_defaults")
    add("Request sampling", "Runtime defaults; no request overrides");
  if (policy.max_tokens_policy === "context_budget_per_request")
    add("Response limit", "Calculated from each request’s remaining context");
  if (typeof thinking === "boolean")
    add("Template thinking override", thinking ? "Enabled" : "Disabled");
  return (
    <details className="task-timings local-model-details">
      <summary>Local model details</summary>
      <dl>
        {rows.map(([label, value]) => (
          <div key={label}>
            <dt>{label}</dt>
            <dd>{value}</dd>
          </div>
        ))}
      </dl>
      <p className="dim">
        File identities are filesystem metadata snapshots, not full weight
        hashes. Header and template hashes cover only the named bytes. Files and
        runtime libraries can change after these observations.
      </p>
      <p className="dim">
        Quantization codes come from the GGUF, not its filename. Runtime
        defaults and backend are reported observations; they do not prove GPU
        offload or identical sampling behavior across models.
      </p>
      {hasToolReport && (
        <p className="dim">
          Runtime template reports do not establish successful tool execution or
          coding quality. Tool schema availability follows the existing model
          policy.
        </p>
      )}
      {!Object.keys(sampling).length && (
        <p className="dim">
          Sampling defaults were not reported by this runtime.
        </p>
      )}
    </details>
  );
}
