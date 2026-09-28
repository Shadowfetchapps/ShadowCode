import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ApiError, readableError } from "./transport";
import { useToasts } from "../hooks/useToasts";

describe("errors shown to people", () => {
  it("an engine error reads as its sentence, without the class name", () => {
    const error = new ApiError("Trust this project before starting a task");
    expect(String(error)).toBe("Trust this project before starting a task");
    expect(`${error}`).toBe("Trust this project before starting a task");
    expect(readableError(error)).toBe(
      "Trust this project before starting a task",
    );
  });

  it("strips a JavaScript prefix from errors and stringified errors", () => {
    expect(readableError(new Error("Only web links can be opened"))).toBe(
      "Only web links can be opened",
    );
    expect(readableError("Error: The folder is gone")).toBe(
      "The folder is gone",
    );
    expect(readableError("ApiError: No route")).toBe("No route");
    expect(readableError("TypeError: x is undefined")).toBe("x is undefined");
    // Ordinary text that mentions an error keeps its words.
    expect(readableError("Build failed: Error: exit 1")).toBe(
      "Build failed: Error: exit 1",
    );
    expect(readableError(null)).toBe("");
  });

  it("notifications drop the prefix that String(error) adds", () => {
    const { result } = renderHook(() => useToasts());
    act(() => result.current.toast(String(new Error("Disk is full")), "err"));
    act(() =>
      result.current.toast(String(new ApiError("Engine stopped")), "err"),
    );
    expect(result.current.toasts.map((t) => t.text)).toEqual([
      "Disk is full",
      "Engine stopped",
    ]);
  });
});
