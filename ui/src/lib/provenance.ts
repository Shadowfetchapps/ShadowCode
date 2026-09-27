import type { LocalRuntimeReceipt } from "../api";

export const provenanceObject = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};

/** Keep the receipt on its recorded task. Old/unknown schemas are not
 * upgraded into provenance, and rendering still validates individual fields. */
export function parseLocalRuntimeReceipt(
  value: unknown,
): LocalRuntimeReceipt | undefined {
  const receipt = provenanceObject(value);
  const runtime = provenanceObject(receipt.runtime);
  const provenance = provenanceObject(runtime.provenance);
  const files = provenanceObject(provenance.files);
  if (
    typeof receipt.model_id !== "string" ||
    provenance.schema !== 1 ||
    provenance.identity_kind !== "filesystem_metadata" ||
    typeof provenanceObject(files.model).path !== "string" ||
    typeof provenanceObject(files.runtime).path !== "string"
  )
    return undefined;
  return receipt as LocalRuntimeReceipt;
}
