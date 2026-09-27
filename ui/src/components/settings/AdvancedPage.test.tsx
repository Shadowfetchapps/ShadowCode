import { afterEach, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { AdvancedPage } from "./AdvancedPage";
afterEach(cleanup);

it("loads and saves the active run limit without changing vendor commands", async () => {
  const onSave = vi.fn(async () => {});
  render(
    <AdvancedPage
      cfg={{
        cli_agents: { max_run_time_sec: 3600, codex_binary: "/opt/codex" },
      }}
      tab="vendors"
      onTab={vi.fn()}
      health={null}
      sessionId=""
      busy={false}
      onSave={onSave}
      onToast={vi.fn()}
      onOpenSession={vi.fn()}
      onSkillsChanged={async () => {}}
      onUseSkill={vi.fn()}
    />,
  );
  const limit = screen.getByLabelText(
    "Maximum active run time (seconds)",
  ) as HTMLInputElement;
  expect(limit.value).toBe("3600");
  fireEvent.change(limit, { target: { value: "0" } });
  expect(
    (screen.getByRole("button", { name: "Save" }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
  fireEvent.change(limit, { target: { value: "14400" } });
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() =>
    expect(onSave).toHaveBeenCalledWith({
      cli_agents: expect.objectContaining({
        max_run_time_sec: 14400,
        codex_binary: "/opt/codex",
      }),
    }),
  );
  expect(
    screen.getByText(/Time paused or waiting for your approval does not count/),
  ).toBeTruthy();
});
