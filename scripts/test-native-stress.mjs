// Sustained native-engine probe. Uses a disposable profile and scripted provider;
// RSS is the engine process, not the Node fixture or an AppImage wrapper.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import {
  mkdtemp,
  mkdir,
  writeFile,
  readFile,
  readdir,
  readlink,
  rm,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { DatabaseSync } from "node:sqlite";
const root = fileURLToPath(new URL("../", import.meta.url));
const binary =
  process.env.SHADOW_STRESS_BINARY ||
  path.join(root, "target/debug/shadowcode");
const artifacts = path.resolve(
  process.env.SHADOW_STRESS_ARTIFACTS ||
    path.join(root, "artifacts/native-stress"),
);
const scratch = await mkdtemp(path.join(tmpdir(), "shadowcode-stress-"));
const profile = path.join(scratch, "profile");
const projects = Array.from({ length: 4 }, (_, i) =>
  path.join(scratch, `project-${i}`),
);
const children = new Set(),
  sockets = new Set(),
  samples = [],
  sequentialSamples = [];
let fixtureError,
  requests = 0,
  hangs = 0,
  server,
  database;
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
await mkdir(artifacts, { recursive: true });
await rm(path.join(artifacts, "result.json"), { force: true });
await rm(path.join(artifacts, "failure.txt"), { force: true });
const env = { ...process.env };
delete env.DISPLAY;
delete env.WAYLAND_DISPLAY;
async function until(label, fn, timeout = 20000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    if (fixtureError) throw fixtureError;
    const result = await fn();
    if (result) return result;
    await delay(50);
  }
  throw Error(`${label} timed out`);
}
function launch(args, workspace = projects[0]) {
  const child = spawn(
    binary,
    ["--profile", profile, "--workspace", workspace, "--json", ...args],
    { env, stdio: ["ignore", "pipe", "pipe"], detached: true },
  );
  children.add(child);
  const output = { stdout: "", stderr: "", code: undefined };
  for (const name of ["stdout", "stderr"])
    child[name].on("data", (chunk) => {
      output[name] += chunk;
      if (output[name].length > 4_000_000) child.kill("SIGKILL");
    });
  const done = new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code) => {
      output.code = code;
      children.delete(child);
      resolve(output);
    });
  });
  return { child, output, done };
}
async function finish(run) {
  await until("CLI completion", () => run.output.code !== undefined, 60000);
  assert.equal(run.output.code, 0, run.output.stderr);
  return JSON.parse(run.output.stdout);
}
async function cli(args, workspace) {
  return finish(launch(args, workspace));
}
const model = createServer(async (req, res) => {
  try {
    let body = "";
    for await (const chunk of req) {
      body += chunk;
      assert.ok(body.length < 2_000_000);
    }
    const payload = JSON.parse(body);
    requests++;
    const index = payload.messages.findLastIndex((m) => m.role === "user");
    const prompt = payload.messages[index].content;
    if (prompt.includes("HANG")) {
      hangs++;
      res.writeHead(200, { "Content-Type": "application/json" });
      res.flushHeaders();
      return;
    }
    const hadTool = payload.messages
      .slice(index + 1)
      .some((m) => m.role === "tool");
    if (hadTool)
      assert.ok(
        payload.messages
          .slice(index + 1)
          .some(
            (m) => m.role === "tool" && m.content.includes("stress-source"),
          ),
      );
    const message = hadTool
      ? {
          role: "assistant",
          content: `${prompt}\n${"Verified evidence. ".repeat(1800)}`,
        }
      : {
          role: "assistant",
          content: "Read the fixture.",
          tool_calls: [
            {
              id: `stress-${requests}`,
              type: "function",
              function: {
                name: "read_file",
                arguments: JSON.stringify({ path: "README.md" }),
              },
            },
          ],
        };
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(
      JSON.stringify({
        choices: [{ message, finish_reason: hadTool ? "stop" : "tool_calls" }],
        usage: {
          prompt_tokens: 400,
          completion_tokens: 800,
          total_tokens: 1200,
        },
      }),
    );
  } catch (error) {
    fixtureError = error;
    res.writeHead(500);
    res.end(String(error));
  }
});
model.on("connection", (socket) => {
  sockets.add(socket);
  socket.on("close", () => sockets.delete(socket));
});
async function sample(round, target = samples) {
  const pid = server.child.pid;
  const status = await readFile(`/proc/${pid}/status`, "utf8");
  const rssKiB = Number(status.match(/^VmRSS:\s+(\d+)/m)?.[1]);
  assert.ok(rssKiB > 0);
  const fds = (await readdir(`/proc/${pid}/fd`)).length;
  const tasks = await readdir(`/proc/${pid}/task`);
  const childPids = (
    await Promise.all(
      tasks.map((t) =>
        readFile(`/proc/${pid}/task/${t}/children`, "utf8").catch((e) => {
          if (e.code === "ENOENT") return "";
          throw e;
        }),
      ),
    )
  )
    .join(" ")
    .trim();
  assert.equal(
    childPids,
    "",
    "Completed work must leave no engine-owned child processes",
  );
  const value = { round, rssKiB, fds, threads: tasks.length };
  target.push(value);
  return value;
}
try {
  await new Promise((resolve) => model.listen(0, "127.0.0.1", resolve));
  await mkdir(path.join(profile, "config"), { recursive: true });
  for (const project of projects) {
    await mkdir(project);
    await writeFile(
      path.join(project, "README.md"),
      "stress-source\n" + "bounded file evidence\n".repeat(5000),
    );
  }
  await writeFile(
    path.join(profile, "config/config.yaml"),
    JSON.stringify({
      model: {
        default: "stress-fixture",
        name: "stress-fixture",
        provider: "local",
        endpoint: `http://127.0.0.1:${model.address().port}/v1`,
        context_limit: 32768,
      },
      trusted_workspaces: projects,
      onboarding: { completed: true, workspace: projects[0] },
      ui: { notify: false },
    }),
  );
  server = launch(["serve"]);
  await until("persistent engine startup", () =>
    server.output.stderr.includes("serving"),
  );
  assert.equal(
    path.basename(await readlink(`/proc/${server.child.pid}/exe`)),
    "shadowcode",
    "Measure the native executable directly",
  );
  const binaryHash = createHash("sha256");
  for await (const chunk of createReadStream(`/proc/${server.child.pid}/exe`))
    binaryHash.update(chunk);
  const binarySha256 = binaryHash.digest("hex");
  const completed = [];
  for (let round = 1; round <= 50; round++) {
    const jobs = await Promise.all(
      projects.map((project, i) =>
        cli(["run", `STRESS round ${round} project ${i}`], project),
      ),
    );
    for (const job of jobs) {
      assert.equal(job.status, "completed");
      completed.push(job.id);
    }
    if (round % 10 === 0) {
      const before = hangs;
      const hanging = await cli(["run", `HANG ${round}`, "--detach"]);
      await until("provider stall reached", () => hangs > before);
      await cli(["jobs", hanging.id, "--cancel"]);
      await until(
        "cancelled stall",
        async () => (await cli(["jobs", hanging.id])).status === "cancelled",
      );
      await sample(round);
      console.log(
        `Completed ${completed.length} tasks; RSS ${samples.at(-1).rssKiB} KiB, ${samples.at(-1).fds} descriptors`,
      );
    }
  }
  // Exercise bounded subprocess output repeatedly in the same persistent owner.
  for (let i = 0; i < 10; i++) {
    const flood = await cli(["exec", "head -c 1048576 /dev/zero | tr '\\0' x"]);
    assert.equal(flood.ok, true);
    assert.equal(flood.exit_code, 0);
    assert.equal(flood.truncated, true);
    assert.ok(flood.stdout.length > 0 && flood.stdout.length < 1048576);
  }
  await sample(51);
  const baseline = samples[0],
    last = samples.at(-1);
  assert.ok(
    last.rssKiB - baseline.rssKiB < 64 * 1024,
    "Engine RSS grew by 64 MiB after warm-up",
  );
  assert.ok(
    Math.max(...samples.map((s) => s.rssKiB)) < 384 * 1024,
    "Engine RSS exceeded 384 MiB",
  );
  assert.ok(
    last.fds <= baseline.fds + 12,
    "File descriptors grew beyond the fixed allowance",
  );
  assert.ok(
    Math.max(...samples.map((point) => point.threads)) <= baseline.threads + 4,
    "Engine thread count grew beyond the fixed allowance",
  );
  // Explicit UI-03 acceptance workload: one fixture task must finish before
  // the next starts. Keep it in addition to the concurrent cross-project run.
  const warmup = await cli(["run", "SEQUENTIAL UI-03 warm-up"]);
  assert.equal(warmup.status, "completed");
  const sequentialBaseline = await sample(0, sequentialSamples);
  const sequentialIds = [];
  for (let index = 1; index <= 100; index++) {
    const task = await cli(["run", `SEQUENTIAL UI-03 fixture ${index}`]);
    assert.equal(task.status, "completed", `Sequential task ${index}`);
    sequentialIds.push(task.id);
    if (index % 25 === 0) {
      const point = await sample(index, sequentialSamples);
      console.log(
        `Sequential UI-03 ${index}/100; RSS ${point.rssKiB} KiB, ${point.fds} descriptors`,
      );
    }
  }
  assert.equal(new Set(sequentialIds).size, 100, "Sequential task IDs must be unique");
  const sequentialLast = sequentialSamples.at(-1);
  assert.ok(
    Math.max(...sequentialSamples.map((point) => point.rssKiB)) < 384 * 1024,
    "Engine RSS exceeded 384 MiB during sequential tasks",
  );
  assert.ok(
    Math.max(...sequentialSamples.map((point) => point.threads)) <= sequentialBaseline.threads + 4,
    "Engine thread count grew during sequential tasks",
  );
  assert.ok(
    sequentialLast.rssKiB - sequentialBaseline.rssKiB < 64 * 1024,
    "Engine RSS grew by 64 MiB during 100 sequential tasks",
  );
  assert.ok(
    sequentialLast.fds <= sequentialBaseline.fds + 12,
    "File descriptors grew during the sequential workload",
  );
  database = new DatabaseSync(path.join(profile, "state/shadow-agent.db"), {
    readOnly: true,
  });
  const rows = database
    .prepare("SELECT payload FROM desktop_jobs")
    .all()
    .map((r) => JSON.parse(r.payload));
  assert.equal(rows.filter((r) => r.status === "completed").length, 301);
  const rowIds = new Set(rows.map((row) => row.id));
  assert.ok(sequentialIds.every((id) => rowIds.has(id)));
  assert.ok(
    sequentialIds.every((id) => rows.find((row) => row.id === id)?.status === "completed"),
    "Every sequential task must have a persisted completed state",
  );
  assert.equal(rows.filter((r) => r.status === "cancelled").length, 5);
  assert.equal(
    rows.filter((r) => ["running", "queued", "cancelling"].includes(r.status))
      .length,
    0,
  );
  assert.equal(
    database.prepare("PRAGMA integrity_check").get().integrity_check,
    "ok",
  );
  database.close();
  database = undefined;
  server.child.kill("SIGTERM");
  await until("engine shutdown", () => server.output.code !== undefined);
  assert.equal(server.output.code, 0);
  // Reopen the persisted profile and retrieve an early task after shutdown.
  assert.equal((await cli(["jobs", completed[0]])).status, "completed");
  await writeFile(
    path.join(artifacts, "result.json"),
    JSON.stringify(
      {
        passed: true,
        binarySha256,
        completed: 301,
        concurrentCompleted: 200,
        sequentialUi03: {
          required: 100,
          completed: sequentialIds.length,
          uniqueIds: new Set(sequentialIds).size,
          samples: sequentialSamples,
          rssGrowthKiB: sequentialLast.rssKiB - sequentialBaseline.rssKiB,
        },
        cancelled: 5,
        subprocessFloods: 10,
        requests,
        samples,
        rssGrowthKiB: last.rssKiB - baseline.rssKiB,
        scope:
          "Native engine with scripted provider; not desktop WebKit or model-server memory",
      },
      null,
      2,
    ),
  );
  console.log("Native sustained stress passed.");
} catch (error) {
  await writeFile(
    path.join(artifacts, "failure.txt"),
    `${error.stack}\n${JSON.stringify(samples)}\n${server?.output.stderr || ""}`,
  );
  throw error;
} finally {
  database?.close();
  for (const child of children) {
    try {
      process.kill(-child.pid, "SIGTERM");
    } catch {}
  }
  await delay(150);
  for (const child of children) {
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch {}
  }
  for (const socket of sockets) socket.destroy();
  await new Promise((resolve) => model.close(resolve));
  await rm(scratch, { recursive: true, force: true });
}
