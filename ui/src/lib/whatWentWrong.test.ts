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

  it("leaves anything else to the text itself", () => {
    expect(kind("The tests still fail: expected 3, received 4.")).toBeNull();
    expect(kind("")).toBeNull();
  });
});
