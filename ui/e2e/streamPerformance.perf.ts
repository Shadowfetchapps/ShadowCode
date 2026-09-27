import { test, expect } from "@playwright/test";
import { cpus, freemem, totalmem, release, platform, loadavg } from "node:os";
import { createHash } from "node:crypto";
import { readFileSync, readdirSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { installFakeBackend } from "./fakeBackend";
import { installFakePerformance } from "./fakePerformance";
import { startPerformanceProfile } from "./performanceProfile";
import { observeMarkdownWorker } from "./observeMarkdownWorker";

test("measure composer response during a long formatted stream and paged history", async ({
  page,
  browser,
}, testInfo) => {
  const fixture = {
    messages: 10_000,
    initialReplyChars: Number(process.env.SHADOW_PERF_INITIAL_CHARS || 32_768),
    intervalMs: 50,
    keys: 200,
    keyIntervalMs: 50,
    markdownMode:
      process.env.SHADOW_PERF_DISABLE_WORKER === "1"
        ? "synchronous control"
        : "worker enabled",
  };
  if (
    !Number.isSafeInteger(fixture.initialReplyChars) ||
    fixture.initialReplyChars < 1 ||
    fixture.initialReplyChars > 250_000
  )
    throw new Error(
      "SHADOW_PERF_INITIAL_CHARS must be an integer from 1 to 250000",
    );
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(installFakeBackend);
  await page.addInitScript(observeMarkdownWorker);
  if (process.env.SHADOW_PERF_DISABLE_WORKER === "1") {
    if (process.env.SHADOW_PERF_REQUIRE_WORKER === "1")
      throw new Error("Cannot require and disable the worker in the same run");
    // Test-only control: exercise the existing unsupported-Worker path with
    // exactly the same app bundle, document, producer, and input measurement.
    await page.addInitScript(() =>
      Object.defineProperty(window, "Worker", {
        value: undefined,
        configurable: true,
      }),
    );
  }
  await page.addInitScript(installFakePerformance, fixture);
  await page.addInitScript(() =>
    (window as any).__SHADOW_PERFORMANCE__.prepareStream(),
  );
  await page.goto("/");
  await page.locator('.task-link[data-session-id="s1"]').click();
  const prompt = page.getByRole("textbox", { name: "Message ShadowCode" });
  await expect(prompt).toBeEnabled();
  await prompt.focus();
  await page.evaluate(() =>
    (window as any).__SHADOW_PERFORMANCE__.startStream(),
  );
  await expect(page.locator(".msg-agent").last()).toContainText(
    "Implementation detail",
  );
  await page.waitForTimeout(1000);
  const finishProfile = await startPerformanceProfile(
    page,
    process.env.SHADOW_PERF_PROFILE,
  );
  await page.evaluate(() => {
    const samples: number[] = [];
    const longTasks: number[] = [];
    const eventTimings: number[] = [];
    const visibleUpdates: number[] = [];
    const reply = document.querySelectorAll(".msg-agent .markdown");
    const visible = new MutationObserver(() =>
      visibleUpdates.push(performance.now()),
    );
    visible.observe(reply.item(reply.length - 1), {
      childList: true,
      characterData: true,
      subtree: true,
    });
    const key = (event: KeyboardEvent) => {
      if (
        event.target instanceof HTMLTextAreaElement &&
        event.key.length === 1
      ) {
        const start = event.timeStamp;
        requestAnimationFrame(() =>
          requestAnimationFrame(() => samples.push(performance.now() - start)),
        );
      }
    };
    document.addEventListener("keydown", key, true);
    const observer = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        if (entry.entryType === "longtask") longTasks.push(entry.duration);
        if (entry.entryType === "event" && entry.name === "keydown")
          eventTimings.push(entry.duration);
      }
    });
    observer.observe({ type: "longtask", buffered: false });
    observer.observe({
      type: "event",
      buffered: false,
      durationThreshold: 16,
    } as PerformanceObserverInit);
    (window as any).__SHADOW_INPUT_METRICS__ = {
      samples,
      longTasks,
      eventTimings,
      visibleUpdates,
      stop: () => {
        document.removeEventListener("keydown", key, true);
        observer.disconnect();
        visible.disconnect();
      },
    };
  });
  const expected = "abcdefghijklmnopqrstuvwxyz"
    .repeat(8)
    .slice(0, fixture.keys);
  const began = Date.now();
  const loadBefore = loadavg();
  for (const key of expected) {
    await page.keyboard.press(key);
    await page.waitForTimeout(fixture.keyIntervalMs);
  }
  await page.waitForTimeout(250);
  await expect(prompt).toHaveValue(expected);
  const metrics = await page.evaluate(() => {
    const scope = window as any;
    scope.__SHADOW_PERFORMANCE__.stopStream();
    scope.__SHADOW_INPUT_METRICS__.stop();
    const { samples, longTasks, eventTimings, visibleUpdates } =
      scope.__SHADOW_INPUT_METRICS__;
    const reply =
      document
        .querySelectorAll(".msg-agent .markdown")
        .item(document.querySelectorAll(".msg-agent .markdown").length - 1)
        ?.textContent || "";
    return {
      samples,
      longTasks,
      eventTimings,
      visibleUpdates,
      stream: scope.__SHADOW_PERFORMANCE__.stats(),
      workers: scope.__SHADOW_MARKDOWN_WORKERS__,
      mountedMessages: document.querySelectorAll(".msg-user, .msg-agent")
        .length,
      domElements: document.querySelectorAll("*").length,
      replyChars: reply.length,
      lastVisibleFragment: Number(
        [...reply.matchAll(/Update (\d+):/g)].at(-1)?.[1] || 0,
      ),
      userAgent: navigator.userAgent,
      devicePixelRatio: devicePixelRatio,
    };
  });
  const percentile = (values: number[], fraction: number) =>
    [...values].sort((a, b) => a - b)[
      Math.max(0, Math.ceil(values.length * fraction) - 1)
    ] ?? null;
  const profile = await finishProfile();
  const sourceFiles = [
    "src/components/Markdown.tsx",
    "src/components/shell/TranscriptRows.tsx",
    "src/hooks/useConversation.ts",
    "src/lib/jobEvents.ts",
    "src/lib/markdownTree.ts",
    "src/lib/markdownWorker.ts",
    "src/workers/markdown.worker.ts",
    "vite.config.ts",
  ];
  const hashFiles = (paths: string[]) =>
    Object.fromEntries(
      paths.map((path) => [
        path,
        createHash("sha256").update(readFileSync(path)).digest("hex"),
      ]),
    );
  const report = {
    scope:
      "Chromium headless browser fixture; not native Wayland, IPC/backend throughput, hardware keyboard latency, or package qualification",
    profile,
    metric:
      "Trusted Playwright keyboard event timestamp to second requestAnimationFrame callback: conservative paint-opportunity proxy, including main-thread queueing; not an actual display-presentation measurement",
    fixture,
    measuredAt: new Date().toISOString(),
    durationMs: Date.now() - began,
    host: {
      cpu: cpus()[0]?.model,
      logicalCpus: cpus().length,
      totalMemoryBytes: totalmem(),
      freeMemoryBytes: freemem(),
      loadAverage: loadavg(),
      loadBefore,
      platform: platform(),
      kernel: release(),
      node: process.version,
      browser: browser.version(),
      viewport: testInfo.project.use.viewport,
      cpuThrottle: "none",
    },
    commit: execFileSync("git", ["rev-parse", "HEAD"], {
      encoding: "utf8",
    }).trim(),
    sourceSha256: hashFiles(sourceFiles),
    fixtureSha256: hashFiles([
      "e2e/fakePerformance.ts",
      "e2e/streamPerformance.perf.ts",
      "e2e/observeMarkdownWorker.ts",
      "e2e/performanceProfile.ts",
      "playwright.performance.config.ts",
    ]),
    builtAssetsSha256: hashFiles(
      readdirSync("dist-e2e/assets")
        .filter((path) => /\.(?:js|css)$/.test(path))
        .sort()
        .map((path) => `dist-e2e/assets/${path}`),
    ),
    input: {
      count: metrics.samples.length,
      p50Ms: percentile(metrics.samples, 0.5),
      p95Ms: percentile(metrics.samples, 0.95),
      maxMs: Math.max(...metrics.samples),
      targetP95Ms: 100,
    },
    eventTiming: {
      count: metrics.eventTimings.length,
      minimumReportedDurationMs: 16,
      p95Ms: percentile(metrics.eventTimings, 0.95),
      caveat:
        "Browser omits events below threshold; this subset is not an all-input p95",
    },
    longTasks: {
      count: metrics.longTasks.length,
      totalMs: metrics.longTasks.reduce(
        (sum: number, value: number) => sum + value,
        0,
      ),
      maxMs: Math.max(0, ...metrics.longTasks),
    },
    stream: metrics.stream,
    workers: metrics.workers,
    streamRendering: {
      visibleUpdates: metrics.visibleUpdates.length,
      lastVisibleFragment: metrics.lastVisibleFragment,
      pendingFragmentsAtCapture:
        metrics.stream.fragments - metrics.lastVisibleFragment,
      caveat:
        "Synthetic producer shares the browser main thread; actual emitted and visible counts are reported. This does not qualify independent native producer throughput or frame presentation.",
    },
    mountedMessages: metrics.mountedMessages,
    domElements: metrics.domElements,
    replyChars: metrics.replyChars,
    userAgent: metrics.userAgent,
    devicePixelRatio: metrics.devicePixelRatio,
    raw: {
      inputMs: metrics.samples,
      longTaskMs: metrics.longTasks,
      eventTimingMs: metrics.eventTimings,
      visibleUpdateTimes: metrics.visibleUpdates,
    },
  };
  const output = resolve(
    process.env.SHADOW_PERF_REPORT ||
      testInfo.outputPath("stream-performance.json"),
  );
  mkdirSync(dirname(output), { recursive: true });
  writeFileSync(output, `${JSON.stringify(report, null, 2)}\n`);
  console.log(
    JSON.stringify({
      report: output,
      input: report.input,
      longTasks: report.longTasks,
      stream: report.stream,
    }),
  );
  expect(errors).toEqual([]);
  expect(metrics.samples).toHaveLength(fixture.keys);
  expect(metrics.stream.fragments).toBeGreaterThan(100);
  expect(metrics.visibleUpdates.length).toBeGreaterThan(50);
  expect(metrics.mountedMessages).toBeLessThanOrEqual(150);
  if (process.env.SHADOW_PERF_REQUIRE_WORKER === "1") {
    expect(metrics.workers.created).toBe(1);
    expect(metrics.workers.errors).toBe(0);
    expect(metrics.workers.trees).toBeGreaterThan(50);
  }
  if (process.env.SHADOW_PERF_DISABLE_WORKER === "1")
    expect(metrics.workers.created).toBe(0);
  // Explicit opt-in qualification, never a flaky timing assertion in default CI.
  expect(report.input.p95Ms).toBeLessThan(100);
});
