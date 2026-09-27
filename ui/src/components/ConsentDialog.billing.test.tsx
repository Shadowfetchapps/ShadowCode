import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ConsentDialog } from "./ConsentDialog";

afterEach(cleanup);

it.each([
  "Billing unverified · API charges may apply",
  "API key login · billed per token",
])(
  "shows %s before cloud consent and keeps Cancel/Send explicit",
  (billingWarning) => {
    const onSend = vi.fn();
    const onCancel = vi.fn();
    const props = {
      request: {
        needs_consent: true as const,
        handoff: { from: "llamacpp", to: "cli:codex", excerpt_chars: 100 },
      },
      destination: "Codex · Default",
      billingWarning,
      attachments: [],
      onSend,
      onCancel,
    };
    render(<ConsentDialog {...props} />);
    expect(screen.getByText(billingWarning)).toBeTruthy();
    expect(onSend).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalledOnce();
    expect(onSend).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(onSend).toHaveBeenCalledOnce();
  },
);
