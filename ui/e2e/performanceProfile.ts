import type { Page } from "@playwright/test";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { loadavg } from "node:os";

/** Opt-in Chromium diagnostics, separate from unprofiled latency receipts. */
export async function startPerformanceProfile(page: Page, output?: string) {
  if (!output) return async () => null;
  const client = await page.context().newCDPSession(page);
  const loadBefore = loadavg();
  await client.send("Profiler.enable");
  await client.send("Profiler.setSamplingInterval", { interval: 1000 });
  await client.send("Profiler.start");
  await client.send("Tracing.start", {
    categories: "devtools.timeline",
    transferMode: "ReturnAsStream",
  });
  return async () => {
    const { profile } = await client.send("Profiler.stop");
    const ended = new Promise<string>((resolve) =>
      client.once("Tracing.tracingComplete", (event) => resolve(event.stream!)),
    );
    await client.send("Tracing.end");
    const handle = await ended;
    let trace = "";
    while (true) {
      const part = await client.send("IO.read", { handle });
      trace += part.data;
      if (part.eof) break;
    }
    await client.send("IO.close", { handle });
    const file = resolve(output);
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(`${file}.cpuprofile`, JSON.stringify(profile));
    writeFileSync(`${file}.trace.json`, trace);
    const nodes = new Map(profile.nodes.map((node) => [node.id, node]));
    const scripts: Record<string, number> = {};
    const functions: Record<string, number> = {};
    for (let i = 0; i < (profile.samples?.length || 0); i++) {
      const frame = nodes.get(profile.samples![i])?.callFrame;
      const ms = (profile.timeDeltas?.[i] || 0) / 1000;
      const url = frame?.url || "(browser/idle)";
      scripts[url] = (scripts[url] || 0) + ms;
      const key = `${frame?.functionName || "(anonymous)"} @ ${url}:${(frame?.lineNumber || 0) + 1}`;
      functions[key] = (functions[key] || 0) + ms;
    }
    const rendering: Record<
      string,
      { count: number; totalMs: number; maxMs: number }
    > = {};
    for (const event of JSON.parse(trace).traceEvents) {
      if (
        !["Layout", "UpdateLayoutTree", "Paint", "CompositeLayers"].includes(
          event.name,
        ) ||
        event.ph !== "X"
      )
        continue;
      const row = (rendering[event.name] ||= {
        count: 0,
        totalMs: 0,
        maxMs: 0,
      });
      const ms = (event.dur || 0) / 1000;
      row.count++;
      row.totalMs += ms;
      row.maxMs = Math.max(row.maxMs, ms);
    }
    const summary = {
      files: [`${file}.cpuprofile`, `${file}.trace.json`],
      loadBefore,
      loadAfter: loadavg(),
      sampledSelfTimeMsByScript: Object.fromEntries(
        Object.entries(scripts).sort((a, b) => b[1] - a[1]),
      ),
      topSelfTimeFunctions: Object.entries(functions)
        .sort((a, b) => b[1] - a[1])
        .slice(0, 30),
      rendering,
      caveat:
        "Sampling/trace collection adds overhead. Script self time is not an inclusive call-tree attribution; layout/style/paint timings may nest and must not be summed as exclusive CPU.",
    };
    writeFileSync(
      `${file}.summary.json`,
      `${JSON.stringify(summary, null, 2)}\n`,
    );
    await client.detach();
    return summary;
  };
}
