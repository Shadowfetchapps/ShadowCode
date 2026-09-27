import { describe, expect, it } from "vitest";
import { cursorCommandOutput } from "./vendorOutput";

const output = () => ({
  cursor_execution: { exit_code: 0 },
  input: { command: "python3 -m unittest -q" },
  content: ["Not in allowlist: python3"],
  raw_output: { exitCode: 0, stdout: "output", stderr: "error stream" },
  raw_output_format: "json",
  raw_output_truncated: false,
  acp_provenance: {
    schema_version: 1,
    history_complete: true,
    content: { source: "permission_request", phase: "pending" },
    raw_output: {
      source: "tool_update",
      phase: "completed",
      explicit_terminal: true,
    },
    permission: {
      state: "resolved",
      decision: "allow_once",
      current_operation_matches: true,
    },
  },
});

describe("Cursor command output provenance", () => {
  it("never labels terminal content as an earlier approval reason", () => {
    const value = output();
    value.acp_provenance.content = {
      source: "tool_update",
      phase: "completed",
    };
    const display = cursorCommandOutput("cursor.execute", value)!;
    expect(display.fullOutput).toContain(
      "Tool content\nNot in allowlist: python3",
    );
    expect(display.fullOutput).not.toContain("Earlier approval reason");
    expect(display.fullOutput).toContain("Standard output\noutput");
    expect(display.fullOutput).toContain("Standard error\nerror stream");
  });
  it("labels retained output and invalidated approval without discarding either", () => {
    const value = output();
    value.acp_provenance.raw_output.explicit_terminal = false;
    value.acp_provenance.permission.current_operation_matches = false;
    const display = cursorCommandOutput("cursor.execute", value)!;
    expect(display.text).toBe(
      "Reported shell result · exit 0 (retained output)",
    );
    expect(display.fullOutput).toContain(
      "Earlier approval does not match the current operation",
    );
    expect(display.fullOutput).toContain("Not in allowlist: python3");
  });
  it("labels incomplete retained history without claiming prior approval was verified", () => {
    const value = output();
    value.acp_provenance.history_complete = false;
    const display = cursorCommandOutput("cursor.execute", value)!;
    expect(display.fullOutput).toContain("History limitation");
    expect(display.fullOutput).toContain("prior approval cannot be verified");
  });
  it("shows pending or declined permission even when terminal content is empty", () => {
    for (const [state, label] of [
      ["pending", "Requested; no client decision recorded"],
      ["declined", "Declined: no usable permission options"],
    ]) {
      const value = output();
      value.content = [];
      value.acp_provenance.permission = {
        state,
        decision: "",
        current_operation_matches: false,
      };
      const display = cursorCommandOutput("cursor.execute", value)!;
      expect(display.fullOutput).toContain(
        `Client permission decision\n${label}`,
      );
      expect(display.fullOutput).not.toContain("Approved once");
    }
  });
  it("requires adapter metadata and keeps unknown providers on the normal output path", () => {
    expect(cursorCommandOutput("grok.execute", output())).toBeUndefined();
    expect(
      cursorCommandOutput("cursor.execute", { raw_output: { exitCode: 0 } }),
    ).toBeUndefined();
    expect(
      cursorCommandOutput("cursor.execute", {
        ...output(),
        acp_provenance: {},
      }),
    ).toBeUndefined();
    expect(
      cursorCommandOutput("cursor.execute", {
        ...output(),
        cursor_execution: { exit_code: "0" },
      }),
    ).toBeUndefined();
  });
});
