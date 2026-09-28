// Real WebKit UI + native command execution; no fake command API or model turn.
// Called after the existing local fixture task and editor/reload checks finish.
import assert from "node:assert/strict";
import { readFile, stat, unlink, writeFile } from "node:fs/promises";
import path from "node:path";

export async function nativeRunCheck(ctx) {
  const composer = 'textarea[aria-label="Message ShadowCode"]';
  const dialog = '[role="dialog"][aria-label="Run a check"]';
  const summaries = 'section[aria-label="Task summary"]';
  const draft = "Keep this unsent draft while the native check runs.";
  const output = "ShadowCode native check passed";
  const projectIdentity = await stat(ctx.project, { bigint: true });
  const cwdIdentity = `${projectIdentity.dev}:${projectIdentity.ino}`;
  // Check the executing process's actual directory without embedding a random
  // absolute path in the durable receipt. Generic secret redaction can mask
  // long mixed-case temporary paths, so receipt.cwd is presentation evidence.
  const command = `test "$(stat -Lc '%d:%i' .)" = '${cwdIdentity}' && test -f hello.txt && printf 'ShadowCode native check passed\\n'`;
  const beforeJobs = await ctx.jobsFor(ctx.sessionId);
  const beforeRequests = (await ctx.modelRequests()).length;
  const beforeFile = await readFile(path.join(ctx.project, "hello.txt"));
  const originalDraft = await ctx.execute("return document.querySelector(arguments[0]).value", [composer]);
  const picker = await ctx.execute("return document.querySelector('.unified-picker-trigger').getAttribute('aria-label')");
  const oldSummary = await ctx.execute("return document.querySelector(arguments[0]).innerText", [summaries]);
  assert.ok(beforeJobs.some(job => job.status === "completed"), "Start from a real completed task");
  assert.equal(await ctx.execute("return document.querySelectorAll(arguments[0]).length", [summaries]), 1);

  await ctx.fill(composer, draft);
  await ctx.clickButton("Run a check…");
  await ctx.until("Native Run a check dialog", () => ctx.visible(dialog));
  assert.equal(await ctx.execute("return document.querySelector(arguments[0]).value", [`${dialog} input`]), "",
    "Historical receipt text is not reused as an executable command");
  assert.ok((await ctx.execute("return document.querySelector(arguments[0]).innerText", [dialog])).includes(ctx.project));
  await ctx.fill(`${dialog} input`, command);
  await ctx.screenshot("run-check-dialog");
  await ctx.clickButton("Run check", "//div[@role='dialog' and @aria-label='Run a check']");
  await ctx.until("Native check dialog closes after admission", async () => !(await ctx.visible(dialog)));
  await ctx.until("Native check requires explicit command approval", () => ctx.execute(
    "return [...document.querySelectorAll('.approval')].some(node => node.textContent.includes(arguments[0]))", [output]));
  const pending = (await ctx.jobsFor(ctx.sessionId)).filter(job => !beforeJobs.some(before => before.id === job.id));
  assert.equal(pending.length, 1, "Exactly one fresh task was admitted");
  assert.equal(pending[0].mode, "command");
  assert.equal(pending[0].model, "native command");
  assert.ok(!["completed", "failed", "cancelled"].includes(pending[0].status), "The check cannot finish before approval");
  assert.equal(await ctx.execute("return document.querySelector(arguments[0]).value", [composer]), draft);
  assert.equal((await ctx.modelRequests()).length, beforeRequests, "Admission invokes no model");
  await ctx.approve(output);

  const job = await ctx.until("Native check completed", async () => {
    const current = (await ctx.jobsFor(ctx.sessionId)).find(candidate => candidate.id === pending[0].id);
    if (["failed", "cancelled"].includes(current?.status)) throw new Error(`Check ${current.status}: ${current.summary}`);
    return current?.status === "completed" && current;
  }, 30000);
  assert.equal(job.workspace, ctx.project);
  assert.equal(job.session_id, ctx.sessionId);
  assert.equal(job.timings?.model_requests, 0, "Native engine confirms zero model requests");
  assert.equal(job.result?.verification?.verified, true);
  const receipt = job.result.verification.commands.find(item => item.command === command);
  assert.ok(receipt, "Persisted result includes this exact command's execution receipt");
  assert.equal(receipt.state, "passed");
  assert.equal(receipt.exit_code, 0);
  assert.equal(receipt.provenance, "locally_observed");
  assert.ok(receipt.cwd === ctx.project || receipt.cwd === "[redacted secret]",
    "Receipt directory is the workspace or its intentional redacted presentation");
  assert.ok(receipt.tool_call_id && receipt.output_ref);

  await ctx.until("A second task summary renders the passed check", () => ctx.execute(`
    const summaries = [...document.querySelectorAll(arguments[0])];
    return summaries.length === 2 && [...summaries[1].querySelectorAll('li')].some(item =>
      item.querySelector('code')?.textContent === arguments[1] && item.querySelector('.ok')?.textContent === 'passed');
  `, [summaries, command]));
  // Expand via the real WebDriver click rather than injecting the open state.
  const receiptElement = await ctx.wd("POST", `/session/${ctx.session}/element`, {
    using: "xpath",
    value: "(//section[@aria-label='Task summary'])[last()]//summary[normalize-space(.)='Execution receipt']",
  });
  await ctx.wd("POST", `/session/${ctx.session}/element/${receiptElement["element-6066-11e4-a52e-4f735466cecf"]}/click`, {});
  await ctx.until("Execution receipt displays actual native stdout", () => ctx.execute(`
    const summary = [...document.querySelectorAll(arguments[0])].at(-1);
    return [...summary.querySelectorAll('details[open] pre')].some(node => {
      try {
        const result = JSON.parse(node.innerText);
        return result.stdout === arguments[1] && result.exit_code === 0;
      } catch { return false; }
    });
  `, [summaries, output + "\n"]));
  assert.equal(await ctx.execute("return document.querySelector(arguments[0]).value", [composer]), draft);
  assert.equal(await ctx.execute("return document.querySelector('.unified-picker-trigger').getAttribute('aria-label')"), picker);
  assert.equal(await ctx.execute("return document.querySelector(arguments[0]).innerText", [summaries]), oldSummary,
    "The earlier task summary remains unchanged");
  assert.deepEqual(await readFile(path.join(ctx.project, "hello.txt")), beforeFile);
  assert.equal((await ctx.modelRequests()).length, beforeRequests, "The entire check invokes no model");
  assert.equal((await ctx.jobsFor(ctx.sessionId)).length, beforeJobs.length + 1);
  await ctx.screenshot("run-check-result");
  // Change a file outside the editor and engine, without refocusing or asking
  // the API to refresh. The visible summary's bounded cadence must detect it.
  const external = path.join(ctx.project, "external-verification-change.txt");
  const changedAt = Date.now();
  await writeFile(external, "External source change after the check\n", { flag: "wx" });
  let externalChangeDetectedMs;
  try {
    await ctx.until("Visible check becomes stale after an external write", () => ctx.execute(`
      const summary = [...document.querySelectorAll(arguments[0])].at(-1);
      return summary?.innerText.includes('Checks are stale — files changed') &&
        !summary.innerText.includes('Configured checks passed');
    `, [summaries]));
    externalChangeDetectedMs = Date.now() - changedAt;
    const retained = (await ctx.jobsFor(ctx.sessionId)).find(current => current.id === job.id);
    assert.deepEqual(retained.result.verification, job.result.verification,
      "Freshness assessment cannot rewrite the original passing receipt");
    assert.equal((await ctx.modelRequests()).length, beforeRequests);
    assert.equal(await ctx.execute("return document.querySelector(arguments[0]).value", [composer]), draft);
    await ctx.screenshot("run-check-external-stale");
  } finally {
    await unlink(external);
  }
  await ctx.until("Restored files reassess against the original check fingerprint", () => ctx.execute(`
    return [...document.querySelectorAll(arguments[0])].at(-1)?.innerText.includes('Configured checks passed');
  `, [summaries]));
  await writeFile(path.join(ctx.artifacts, "native-run-check.json"), JSON.stringify({
    passed: true, task_id: job.task_id, job_id: job.id, command, state: receipt.state,
    exit_code: receipt.exit_code, provenance: receipt.provenance, model_requests: job.timings.model_requests,
    output_observed_in_receipt: output, cwd_device_inode_checked: cwdIdentity,
    receipt_cwd_redacted: receipt.cwd === "[redacted secret]",
    draft_preserved: true, earlier_summary_preserved: true,
    external_change_detected_ms: externalChangeDetectedMs,
    original_receipt_preserved_after_external_edit: true,
  }, null, 2) + "\n");
  await ctx.fill(composer, originalDraft);
  ctx.note("Run a check: real command approval, native output/receipt, preserved draft and earlier summary, zero model requests");
  ctx.note("Visible check evidence refreshes after external writes without focus changes or model requests");
}
