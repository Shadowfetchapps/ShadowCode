import { describe, expect, it } from "vitest";
import { whatWentWrong } from "./whatWentWrong";

const kind = (text: string) => whatWentWrong(text)?.kind ?? null;

describe("what went wrong", () => {
  it("names the common failures from the engine's own text", () => {
    expect(
      kind(
        "Model provider returned HTTP 401; check the API key: No auth credentials found",
      ),
    ).toBe("key");
    expect(
      kind(
        "Model provider returned HTTP 402; the provider account needs credits or a payment method: This request requires more credits",
      ),
    ).toBe("credits");
    expect(
      kind(
        "Model provider returned HTTP 404; check the endpoint and model name: No endpoints found for acme/gone",
      ),
    ).toBe("model");
    expect(
      kind("Model provider returned HTTP 429; provider rate limit reached"),
    ).toBe("rate");
    expect(kind("Model provider returned HTTP 529; provider overloaded")).toBe(
      "overloaded",
    );
    expect(
      kind(
        "Model provider returned HTTP 400: This model's maximum context length is 32768 tokens",
      ),
    ).toBe("context");
    expect(
      kind(
        "ShadowCode hit an internal error and stopped this task. Your files and this conversation were kept; you can send the message again.",
      ),
    ).toBe("internal");
  });

  it("tells a stopped local model from being offline", () => {
    const local = whatWentWrong(
      "Could not connect to the model provider: error sending request for url (http://127.0.0.1:8080/v1/chat/completions): Connection refused",
    );
    expect(local?.kind).toBe("local-down");
    expect(local?.steps[0]).toBe("open-local");
    expect(
      kind(
        "Could not connect to the model provider: error sending request for url (https://openrouter.ai/api/v1/chat/completions): dns error",
      ),
    ).toBe("offline");
  });

  it("doesn't offer waiting for a used-up daily limit or quota", () => {
    for (const text of [
      "Model provider returned HTTP 429; provider rate limit reached: Rate limit exceeded: free-models-per-day. Add 10 credits to unlock 1000 free model requests per day",
      "Model provider returned HTTP 429; provider rate limit reached: You exceeded your current quota, please check your plan and billing details",
    ]) {
      const diagnosis = whatWentWrong(text);
      expect(diagnosis?.kind).toBe("quota");
      expect(diagnosis?.steps).not.toContain("retry");
    }
    // A per-minute limit still passes on its own.
    expect(
      kind(
        "Model provider returned HTTP 429; provider rate limit reached: Rate limit exceeded: free-models-per-min.",
      ),
    ).toBe("rate");
  });

  it("blames a provider's internal error on the provider", () => {
    const stream = whatWentWrong(
      "Provider reported an error while generating: Internal error encountered.",
    );
    expect(stream?.kind).toBe("provider-error");
    expect(stream?.steps).toEqual(["retry", "try-on"]);
    expect(kind("Model provider returned HTTP 500: Internal error")).toBe(
      "overloaded",
    );
    expect(kind("codex exited: internal error")).toBeNull();
  });

  it("points an internal failure at a control that exists", () => {
    const internal = whatWentWrong(
      "ShadowCode hit an internal error and stopped this task. Your files and this conversation were kept; you can send the message again.",
    );
    expect(internal?.plain).toContain(
      "Save diagnostics… in Settings › Advanced › Health",
    );
    expect(internal?.plain).not.toContain("Export a health report");
  });

  it("doesn't promise a local server error passes on its own", () => {
    const local = whatWentWrong(
      "Model provider returned HTTP 500; the model server on this computer failed: Value is not callable: null at row 1, column 72",
    );
    expect(local?.kind).toBe("local-error");
    expect(local?.steps).toEqual(["open-local", "try-on"]);
    expect(local?.plain).not.toContain("passes on its own");
    // A local model still loading (503) is temporary.
    expect(
      kind(
        "Model provider returned HTTP 503; provider overloaded: Loading model",
      ),
    ).toBe("overloaded");
    // 501 is never temporary.
    expect(kind("Model provider returned HTTP 501")).toBeNull();
  });

  it("leaves anything else to the text itself", () => {
    expect(kind("The tests still fail: expected 3, received 4.")).toBeNull();
    expect(kind("")).toBeNull();
  });
});
