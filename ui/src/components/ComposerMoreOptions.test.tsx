import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ComposerMoreOptions } from "./ComposerMoreOptions";

afterEach(cleanup);

describe("composer more options", () => {
  it("marks a non-default reasoning effort while the menu is closed", () => {
    const { container } = render(
      <ComposerMoreOptions indicator="High">
        <button type="button">Compare models</button>
      </ComposerMoreOptions>,
    );
    const summary = container.querySelector("summary")!;
    expect(summary.textContent).toContain("High");
    expect(summary.getAttribute("aria-label")).toMatch(/reasoning effort High/);
  });

  it("keeps advanced actions collapsed until opened and closes after an action", () => {
    const onCompare = vi.fn();
    const { container } = render(
      <ComposerMoreOptions>
        <button type="button" aria-disabled="true">
          Worktree unavailable
        </button>
        <button type="button" onClick={onCompare}>
          Compare models
        </button>
      </ComposerMoreOptions>,
    );
    const disclosure = container.querySelector(
      "details",
    ) as HTMLDetailsElement | null;
    expect(disclosure?.open).toBe(false);

    fireEvent.click(container.querySelector("summary")!);
    expect(disclosure?.open).toBe(true);
    fireEvent.click(
      screen.getByRole("button", { name: "Worktree unavailable" }),
    );
    expect(disclosure?.open).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Compare models" }));

    expect(onCompare).toHaveBeenCalledOnce();
    expect(disclosure?.open).toBe(false);
  });

  it("closes on Escape and when the user clicks outside", () => {
    const { container } = render(
      <>
        <ComposerMoreOptions>
          <button type="button">Compare models</button>
        </ComposerMoreOptions>
        <button type="button">Outside</button>
      </>,
    );
    const disclosure = container.querySelector("details") as HTMLDetailsElement;
    const summary = disclosure.querySelector("summary")!;
    fireEvent.click(summary);
    fireEvent.keyDown(screen.getByRole("button", { name: "Compare models" }), {
      key: "Escape",
    });
    expect(disclosure.open).toBe(false);
    expect(document.activeElement).toBe(summary);

    fireEvent.click(summary);
    fireEvent.pointerDown(screen.getByRole("button", { name: "Outside" }));
    expect(disclosure.open).toBe(false);
  });
});
