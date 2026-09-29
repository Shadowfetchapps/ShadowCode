import { describe, expect, it } from "vitest";
import {
  clockTime,
  money,
  parseLimit,
  parseRunRecord,
  retryText,
  runDetailRows,
} from "./spending";

describe("spending helpers", () => {
  it("formats money and limits plainly", () => {
    expect(money(1)).toBe("$1.00");
    expect(money(0.004)).toBe("less than $0.01");
    expect(parseLimit("")).toBeNull();
    expect(parseLimit("$2.5")).toBe(2.5);
    expect(parseLimit("0")).toBe("invalid");
    expect(parseLimit("abc")).toBe("invalid");
  });

  it("names the wait and the attempt for retries", () => {
    expect(
      retryText({
        attempt: 2,
        max_attempts: 5,
        reason: "rate_limited",
        delay_ms: 3600,
      }),
    ).toBe("Provider busy, retrying (2 of 5) in 4 s…");
    expect(
      retryText({
        attempt: 1,
        max_attempts: 3,
        reason: "stalled",
        delay_ms: 200,
      }),
    ).toBe("Connection to the provider dropped, retrying (1 of 3) now…");
  });

  it("says tomorrow for a reset after midnight", () => {
    const now = new Date(2026, 8, 29, 22, 0).getTime() / 1000;
    const later = new Date(2026, 8, 29, 23, 30).getTime() / 1000;
    const tomorrow = new Date(2026, 8, 30, 9, 15).getTime() / 1000;
    expect(clockTime(later, now)).not.toContain("tomorrow");
    expect(clockTime(tomorrow, now)).toMatch(/^tomorrow at /);
  });

  it("reads a run record and lists it for Run details", () => {
    expect(parseRunRecord(null)).toBeUndefined();
    expect(parseRunRecord({ model_id: "x" })).toBeUndefined();
    const run = parseRunRecord({
      model_id: "api:openrouter:qwen/qwen3-coder",
      model: "qwen/qwen3-coder",
      provider: "openrouter",
      route: "native_http",
      effort: null,
      app_version: "1.0.0",
      app_commit: "0123456789abcdef",
      settings_hash: "abc123abc123",
      rules_hash: "def456def456",
    })!;
    const rows = Object.fromEntries(runDetailRows(run));
    expect(rows.Model).toBe("api:openrouter:qwen/qwen3-coder");
    expect(rows.Effort).toBe("Model default");
    expect(rows.ShadowCode).toBe("1.0.0 (0123456789ab)");
    expect(rows["Rules and skills"]).toBe("def456def456");
    expect(rows["Ran with"]).toBeUndefined();
  });
});
