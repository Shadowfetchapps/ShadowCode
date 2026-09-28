import { describe, expect, it } from "vitest";
import { applyEvent, emptyTranscript } from "./transcript";
import type { EventRow } from "../api";

const event = (
  id: number,
  type: string,
  payload: Record<string, unknown>,
): EventRow => ({
  id,
  ts: id,
  type,
  payload,
  task_id: "scale",
});

function replay(count: number) {
  const started = performance.now();
  let state = emptyTranscript();
  for (let i = 1; i <= count; i++) {
    state = applyEvent(state, event(i, "model.delta", { text: String(i) }));
  }
  return { state, ms: performance.now() - started };
}

describe("extreme transcript reconstruct", () => {
  it(
    "measures 10k / 50k / 100k reducer cost without virtualizing",
    { timeout: 120_000 },
    () => {
      const env = (
        globalThis as { process?: { env?: { QUAL_SCALE?: string } } }
      ).process?.env;
      if (!env?.QUAL_SCALE) {
        expect(replay(10_000).state.cursor).toBe(10_000);
        return;
      }
      const ten = replay(10_000);
      expect(ten.state.cursor).toBe(10_000);
      expect(ten.state.items).toHaveLength(10_000);
      const fifty = replay(50_000);
      expect(fifty.state.cursor).toBe(50_000);
      expect(fifty.state.items.at(-1)?.text).toBe("50000");
      const hundred = replay(100_000);
      expect(hundred.state.cursor).toBe(100_000);
      expect(hundred.state.items).toHaveLength(100_000);
      expect(hundred.state.items[99_999].text).toBe("100000");
      // eslint-disable-next-line no-console
      console.log(
        JSON.stringify({
          reconstruct_10k_ms: Math.round(ten.ms),
          reconstruct_50k_ms: Math.round(fifty.ms),
          reconstruct_100k_ms: Math.round(hundred.ms),
          virtualized: false,
        }),
      );
      expect(ten.ms).toBeLessThan(8_000);
      expect(fifty.ms).toBeLessThan(40_000);
      expect(hundred.ms).toBeLessThan(90_000);
    },
  );
});
