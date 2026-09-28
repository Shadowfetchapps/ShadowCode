import { randomUUID } from "node:crypto";
import { spawnSync } from "node:child_process";

const ownershipLabel = "io.shadowcode.clean-host-run";
const describe = (result) =>
  `(${result.status}): ${result.error || ""}\n${result.stdout || ""}\n${result.stderr || ""}`;

// Killing an attached Docker/Podman client does not stop its container. Bind
// cleanup to a private invocation label, verify the name, then remove only the
// immutable ID that the runtime returned. Every CLI call has a hard deadline.
// These queries establish observed daemon state, not a transaction barrier:
// after client termination an already submitted create may still be pending.
// Disabling pulls reduces that window but cannot prove delayed creation absent.
export function runCleanHostContainer({
  executable,
  args,
  label,
  timeout = 120_000,
  cleanupTimeout = 10_000,
  env = process.env,
}) {
  const token = randomUUID();
  const name = `shadowcode-clean-${token}`;
  const invoke = (command, deadline, maxBuffer = 2 * 1024 * 1024) =>
    spawnSync(executable, command, {
      encoding: "utf8",
      timeout: deadline,
      killSignal: "SIGKILL",
      maxBuffer,
      env,
    });
  const checked = (command) => {
    const result = invoke(command, cleanupTimeout);
    if (result.error || result.status !== 0)
      throw new Error(
        `Container cleanup command ${command[0]} failed ${describe(result)}`,
      );
    return result.stdout;
  };
  const ownedIds = () => {
    const output = checked([
      "ps",
      "--all",
      "--quiet",
      "--no-trunc",
      "--filter",
      `label=${ownershipLabel}=${token}`,
    ]).trim();
    const ids = output ? output.split(/\s+/) : [];
    if (ids.length > 1 || ids.some((id) => !/^[a-f0-9]{64}$/.test(id)))
      throw new Error(
        "Container cleanup received ambiguous or invalid owned IDs",
      );
    return ids;
  };
  const cleanup = () => {
    for (const id of ownedIds()) {
      const inspection = invoke(
        ["inspect", "--format", "{{json .}}", id],
        cleanupTimeout,
      );
      if (inspection.error || inspection.status !== 0) {
        // Auto-removal can finish between ps and inspect. Only an ordinary
        // failed inspect plus a successful empty query establishes that race;
        // a timed-out inspection remains a cleanup health failure.
        if (!inspection.error && ownedIds().length === 0) continue;
        throw new Error(
          `Container cleanup inspection failed ${describe(inspection)}`,
        );
      }
      const inspected = JSON.parse(inspection.stdout);
      if (
        inspected.Id !== id ||
        inspected.Name?.replace(/^\//, "") !== name ||
        inspected.Config?.Labels?.[ownershipLabel] !== token
      )
        throw new Error(
          "Container cleanup refused an identity/ownership mismatch",
        );
      // --rm may race us after inspection. A failed rm is harmless only when a
      // successful follow-up query proves this invocation owns no container.
      const removed = invoke(["rm", "--force", id], cleanupTimeout);
      const remaining = ownedIds();
      if (remaining.length)
        throw new Error(
          `Owned container ${id} remains after cleanup ${describe(removed)}`,
        );
      if (removed.error)
        // Even after observed disappearance, a hung/failed client is a gate
        // health failure. Do not describe its removal command as successful.
        throw new Error(
          `Container cleanup could not finish within its deadline ${describe(removed)}`,
        );
    }
  };
  let result;
  let cleanupError;
  try {
    result = invoke(
      [
        "run",
        "--pull=never",
        "--rm",
        "--name",
        name,
        "--label",
        `${ownershipLabel}=${token}`,
        ...args,
      ],
      timeout,
    );
  } finally {
    try {
      cleanup();
    } catch (error) {
      cleanupError = error;
    }
  }
  if (result.error || result.status !== 0 || cleanupError)
    throw new Error(
      `${label} failed ${describe(result)}${cleanupError ? `\n${cleanupError.message}` : ""}${result.error ? `\nCleanup checked observed daemon state; delayed creation is not excluded. Invocation: ${name} (${ownershipLabel}=${token}).` : ""}`,
    );
  return result.stdout;
}
