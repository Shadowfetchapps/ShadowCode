// ACP merges omitted fields. Cursor's shell completion omits content, leaving
// a prior permission reason beside a new raw result. Only adapter-owned typed
// provenance may label that reason as earlier; historical JSON stays intact.
const record = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;

export function cursorCommandOutput(tool: string, value: unknown) {
  if (tool !== "cursor.execute") return undefined;
  const output = record(value);
  const code = record(output?.cursor_execution)?.exit_code;
  if (typeof code !== "number" || !Number.isSafeInteger(code)) return undefined;
  const provenance = record(output?.acp_provenance);
  if (provenance?.schema_version !== 1) return undefined;
  const rawOrigin = record(provenance.raw_output);
  const terminal = rawOrigin?.explicit_terminal === true;
  const text = `${terminal ? "Command result" : "Reported shell result"} · exit ${code}${terminal ? "" : " (retained output)"}`;
  const sections = [text];
  if (provenance.history_complete !== true) {
    sections.push(
      "History limitation\nEarlier tool or approval history was not retained; prior approval cannot be verified.",
    );
  }
  const command = record(output?.input)?.command;
  if (typeof command === "string") sections.push(`Command\n${command}`);
  const content = Array.isArray(output?.content)
    ? output.content.filter((item): item is string => typeof item === "string")
    : [];
  if (content.length) {
    const origin = record(provenance.content);
    const earlier =
      origin?.source === "permission_request" && origin.phase === "pending";
    sections.push(
      `${earlier ? "Earlier approval reason" : "Tool content"}\n${content.join("\n")}`,
    );
  }
  const permission = record(provenance.permission);
  if (permission) {
    const decision = permission.decision;
    const label =
      permission.state === "pending"
        ? "Requested; no client decision recorded"
        : permission.state === "declined"
          ? "Declined: no usable permission options"
          : permission.state !== "resolved"
            ? "Unknown permission state"
            : permission.current_operation_matches !== true
              ? "Earlier approval does not match the current operation"
              : decision === "allow_once"
                ? "Approved once"
                : decision === "allow_always"
                  ? "Approved by the selected persistent option"
                  : decision === "reject_once" || decision === "reject_always"
                    ? "Rejected"
                    : decision === "cancelled"
                      ? "Cancelled"
                      : "Unknown decision";
    sections.push(`Client permission decision\n${label}`);
  }
  const raw = record(output?.raw_output);
  if (
    output?.raw_output_format === "json" &&
    output.raw_output_truncated === false &&
    raw
  ) {
    if (typeof raw.stdout === "string" && raw.stdout)
      sections.push(`Standard output\n${raw.stdout}`);
    if (typeof raw.stderr === "string" && raw.stderr)
      sections.push(`Standard error\n${raw.stderr}`);
  } else {
    sections.push(
      `Raw output preview (truncated)\n${typeof output?.raw_output === "string" ? output.raw_output : JSON.stringify(output?.raw_output)}`,
    );
  }
  return { text, fullOutput: sections.join("\n\n"), failed: code !== 0 };
}
