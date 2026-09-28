import { describe, expect, it } from "vitest";
import type { DownloadCatalog, DownloadModel } from "../api";
import {
  blockedReason,
  downloadEstimate,
  formatDuration,
  formatSize,
  hardwareSummary,
  inProgress,
  noRecommendationReason,
  offered,
  percent,
  progressText,
  recommendedModel,
} from "./downloads";

function model(over: Partial<DownloadModel> = {}): DownloadModel {
  return {
    id: "gemma-4-e4b",
    name: "Gemma 4 E4B",
    publisher: "Google",
    summary: "Balanced.",
    file: "gemma.gguf",
    bytes: 5_154_941_280,
    sha256: "0".repeat(64),
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
    source_url: "https://huggingface.co/x",
    quantization: "Q4_0",
    architecture: "gemma4",
    memory_bytes: 6_503_410_016,
    min_memory_bytes: 6_151_088_480,
    fit: "gpu",
    recommended: true,
    supported: true,
    state: "available",
    done: 0,
    total: 5_154_941_280,
    bytes_per_second: 0,
    ...over,
  };
}

function catalog(over: Partial<DownloadCatalog> = {}): DownloadCatalog {
  return {
    directory: "/data/local-models",
    free_bytes: 100e9,
    offline: false,
    hardware: { ram_bytes: 16 * 1024 ** 3, vram_bytes: null, gpu: null },
    recommended: "gemma-4-e4b",
    recommended_fit: "cpu",
    busy: false,
    models: [model()],
    ...over,
  };
}

describe("download wording", () => {
  it("sizes and durations read like a download page", () => {
    expect(formatSize(5_154_941_280)).toBe("4.8 GB");
    expect(formatSize(20_419_565_568)).toBe("19 GB");
    expect(formatSize(836_000_000)).toBe("797 MB");
    expect(formatSize(0)).toBe("0 MB");
    expect(formatDuration(20)).toBe("less than a minute");
    expect(formatDuration(60)).toBe("about 1 minute");
    expect(formatDuration(6 * 60 + 10)).toBe("about 6 minutes");
    expect(formatDuration(70 * 60)).toBe("about 1 hour 10 minutes");
    expect(formatDuration(120 * 60)).toBe("about 2 hours");
    // 4.8 GB (5.2 billion bytes) at 50 Mbit/s is about 14 minutes.
    expect(downloadEstimate(5_154_941_280)).toBe(
      "about 14 minutes on a 50 Mbit/s connection",
    );
  });

  it("progress lines follow the download state", () => {
    const half = { done: 2_577_470_640 };
    expect(
      progressText(
        model({ state: "downloading", ...half, bytes_per_second: 25e6 }),
      ),
    ).toBe("2.4 GB of 4.8 GB · about 2 minutes left");
    expect(progressText(model({ state: "downloading", ...half }))).toBe(
      "2.4 GB of 4.8 GB · starting…",
    );
    expect(progressText(model({ state: "checking", ...half }))).toBe(
      "Checking the 2.4 GB already downloaded…",
    );
    expect(progressText(model({ state: "paused", ...half }))).toBe(
      "Paused at 2.4 GB of 4.8 GB",
    );
    expect(progressText(model({ state: "failed", ...half }))).toBe(
      "Stopped at 2.4 GB of 4.8 GB",
    );
    expect(progressText(model({ state: "failed" }))).toBe("Download stopped");
    expect(percent(model({ ...half }))).toBe(50);
    expect(inProgress(model({ state: "failed" }))).toBe(true);
    expect(inProgress(model({ state: "installed" }))).toBe(false);
    expect(inProgress(model())).toBe(false);
  });

  it("explains why Download can't be pressed", () => {
    expect(blockedReason(model(), catalog())).toBe("");
    expect(blockedReason(model(), catalog({ offline: true }))).toMatch(
      /^Offline mode is on/,
    );
    expect(blockedReason(model({ fit: "no" }), catalog())).toBe(
      "Needs more memory than this computer has",
    );
    expect(
      blockedReason(
        model({ supported: false, unsupported_reason: "No gemma4" }),
        catalog(),
      ),
    ).toBe("No gemma4");
    expect(blockedReason(model(), catalog({ busy: true }))).toMatch(
      /Another model is downloading/,
    );
    // The running one itself isn't blocked by being busy.
    expect(
      blockedReason(model({ state: "downloading" }), catalog({ busy: true })),
    ).toBe("");
  });

  it("picks the recommendation and what to offer", () => {
    const big = model({ id: "big", fit: "no", recommended: false });
    const odd = model({ id: "odd", supported: false, recommended: false });
    const all = catalog({ models: [model(), big, odd] });
    expect(recommendedModel(all)?.id).toBe("gemma-4-e4b");
    expect(recommendedModel(null)).toBeUndefined();
    expect(offered(all).map((m) => m.id)).toEqual(["gemma-4-e4b"]);
    expect(
      hardwareSummary(
        catalog({
          hardware: {
            ram_bytes: 62 * 1024 ** 3 + 5e8,
            vram_bytes: 16_557 * 1024 ** 2,
            gpu: "NVIDIA GeForce RTX 5060 Ti",
          },
        }),
      ),
    ).toBe("NVIDIA GeForce RTX 5060 Ti (16 GB) · 62 GB memory");
    expect(formatSize(6_503_410_016)).toBe("6.1 GB");
    expect(formatSize(8 * 1024 ** 3)).toBe("8.0 GB");
    expect(
      noRecommendationReason(
        catalog({
          recommended: null,
          hardware: { ram_bytes: 4 * 1024 ** 3, vram_bytes: null, gpu: null },
          models: [model({ fit: "no", min_memory_bytes: 3_384_862_240 })],
        }),
      ),
    ).toBe(
      "This computer has 4.0 GB of memory; the smallest free model needs about 5.2 GB. Use an OpenRouter key or a subscription instead.",
    );
  });
});
