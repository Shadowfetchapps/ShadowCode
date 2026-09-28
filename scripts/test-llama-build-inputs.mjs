import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { chmod, mkdtemp, mkdir, readFile, readlink, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { promisify } from "node:util";

const exec = promisify(execFile);
const oldTime = "2020-01-01T00:00:00Z";
const buildTime = "2026-09-28T12:34:56Z";
const inputPaths = ["tools/llama.cpp.pin", "packaging/llama.cpp/COMMIT"];
// Allows the regression to be demonstrated against a saved pre-fix script.
const builder = await readFile(process.env.SHADOW_TEST_LLAMA_BUILD_SCRIPT || new URL("./build-llama.cpp.sh", import.meta.url));
const git = (cwd, ...args) => exec("git", ["-c", "core.hooksPath=/dev/null", "-c", "user.name=Build Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", ...args], { cwd });

async function fixture() {
  const root = await mkdtemp(path.join(tmpdir(), "shadowcode-build-inputs-"));
  async function put(relative, contents, executable = false) {
    const destination = path.join(root, relative);
    await mkdir(path.dirname(destination), { recursive: true });
    await writeFile(destination, contents);
    if (executable) await chmod(destination, 0o755);
  }
  try {
    const source = path.join(root, "tools/llama.cpp");
    const notices = {
      LICENSE: "Fixture llama.cpp license\n",
      "licenses/LICENSE-jsonhpp": "Fixture JSON license\n",
      "vendor/cpp-httplib/LICENSE": "Fixture HTTP license\n",
      "vendor/stb/stb_image.h": "ignored header\nThis software is available under 2 licenses\nFixture stb license\n",
      "vendor/miniaudio/miniaudio.h": "ignored header\nThis software is available as a choice of the following licenses\nFixture audio license\n",
      "vendor/sheredom/subprocess.h": "Fixture subprocess license\nFor more information, please refer to upstream.\n",
      "src/llama-arch.cpp": 'const auto LLM_ARCH_NAMES = {\n{ LLM_ARCH_QWEN3, "qwen3" },\n{ LLM_ARCH_LLAMA, "llama" },\n};\n',
    };
    for (const [file, contents] of Object.entries(notices)) await put(`tools/llama.cpp/${file}`, contents);
    await git(source, "init", "-q");
    await git(source, "add", ".");
    await git(source, "commit", "-qm", "Pinned fixture upstream");
    await git(source, "remote", "add", "origin", source);
    const commit = (await git(source, "rev-parse", "HEAD")).stdout.trim();
    const metadata = `url=${source}\ncommit=${commit}\nspirv_headers_commit=${"a".repeat(40)}\nbackend=vulkan+cpu\nbuilt=${oldTime}\n`;
    await put(inputPaths[0], `# Checked-in source pin must remain byte-identical.\n${metadata}`);
    await put(inputPaths[1], `# Checked-in packaging manifest is a source input.\n${metadata}`);
    await put("scripts/build-llama.cpp.sh", builder, true);
    await put(".gitignore", "/tools/llama.cpp/\n/tools/fixture-bin/\n/packaging/llama.cpp/bin/\n/user-runtime/\n");
    await put("tools/fixture-bin/date", `#!/bin/sh\nprintf '%s\\n' '${buildTime}'\n`, true);
    // Only the compiler/linker boundary is replaced. Actual Git checkout,
    // Bash branching, install/copy/symlink handling, notices and metadata run.
    await put("tools/fixture-bin/cmake", `#!${process.execPath}
const fs=require('node:fs'),path=require('node:path');
const args=process.argv.slice(2),root=path.resolve(__dirname,'../..');
fs.appendFileSync(path.join(__dirname,'cmake.jsonl'),JSON.stringify(args)+'\\n');
if(args[0]==='--build') {
  const bin=path.join(args[1],'bin');fs.mkdirSync(bin,{recursive:true});
  for(const name of ['llama-cli','llama-server']) fs.writeFileSync(path.join(bin,name),'#!/bin/sh\\nprintf "fixture runtime version\\\\n"\\n',{mode:0o755});
  fs.writeFileSync(path.join(bin,'libggml.so.1'),'fixture shared library');
  fs.symlinkSync('libggml.so.1',path.join(bin,'libggml.so'));
}
`, true);
    await put("tools/fixture-bin/patchelf", "#!/bin/sh\nexit 0\n", true);
    await git(root, "init", "-q");
    await git(root, "add", ".");
    await git(root, "commit", "-qm", "Immutable build inputs");
    const originals = await Promise.all(inputPaths.map(file => readFile(path.join(root, file))));
    const env = { ...process.env, PATH: `${path.join(root, "tools/fixture-bin")}:${process.env.PATH}`, SHADOWCODE_CMAKE: path.join(root, "tools/fixture-bin/cmake"), SHADOWCODE_LLAMA_SRC: source, SHADOWCODE_LLAMA_BUILD_DIR: path.join(source, "build"), SHADOWCODE_LLAMA_URL: source, SHADOWCODE_LLAMA_COMMIT: commit, SHADOWCODE_SPIRV_HEADERS_COMMIT: "a".repeat(40), SHADOWCODE_LLAMA_PREFIX: path.join(root, "user-runtime"), SHADOWCODE_BUILD_JOBS: "1" };
    return {
      root, commit, source, put,
      run: args => exec("bash", [path.join(root, "scripts/build-llama.cpp.sh"), ...args], { cwd: root, env, timeout: 15000 }),
      async unchanged() {
        for (const [index, file] of inputPaths.entries()) assert((await readFile(path.join(root, file))).equals(originals[index]), `${file} remains byte-identical`);
        assert.equal((await git(root, "status", "--porcelain", "--untracked-files=no")).stdout, "", "The real shell flow leaves the tracked source checkout clean");
      },
      dispose: () => rm(root, { recursive: true, force: true }),
    };
  } catch (error) { await rm(root, { recursive: true, force: true }); throw error; }
}

test("ordinary llama build writes new metadata only to ignored runtime output", async () => {
  const f = await fixture();
  try {
    await f.run(["--cpu-only", "--no-user-install"]);
    const generated = await readFile(path.join(f.root, "packaging/llama.cpp/bin/COMMIT"), "utf8");
    assert.match(generated, new RegExp(`^commit=${f.commit}$`, "m"));
    assert.match(generated, /^backend=cpu$/m);
    assert.match(generated, new RegExp(`^built=${buildTime}$`, "m"));
    assert.doesNotMatch(generated, new RegExp(oldTime));
    const calls = (await readFile(path.join(f.root, "tools/fixture-bin/cmake.jsonl"), "utf8")).trim().split("\n").map(JSON.parse);
    assert.equal(calls.length, 2);
    assert(calls[0].includes("-DGGML_VULKAN=OFF"));
    assert.equal(calls[1][0], "--build");
    assert.equal(await readlink(path.join(f.root, "packaging/llama.cpp/bin/libggml.so")), "libggml.so.1");
    assert.equal(await readFile(path.join(f.root, "packaging/llama.cpp/bin/NOTICES/llama.cpp-LICENSE"), "utf8"), "Fixture llama.cpp license\n");
    assert.equal((await git(f.root, "check-ignore", "packaging/llama.cpp/bin/COMMIT")).stdout.trim(), "packaging/llama.cpp/bin/COMMIT");
    await f.unchanged();
  } finally { await f.dispose(); }
});

for (const matching of [true, false]) {
  test(`notices-only ${matching ? "refreshes ignored output without rewriting tracked metadata" : "refuses a mismatched built commit without rewriting metadata"}`, async () => {
    const f = await fixture();
    try {
      const existing = `url=${f.source}\ncommit=${matching ? f.commit : "b".repeat(40)}\nbackend=cpu\nbuilt=2024-03-02T01:02:03Z\n`;
      await f.put("packaging/llama.cpp/bin/llama-server", "#!/bin/sh\nexit 0\n", true);
      await f.put("packaging/llama.cpp/bin/COMMIT", existing);
      await f.put("packaging/llama.cpp/bin/NOTICES/llama.cpp-LICENSE", "stale notice\n");
      if (matching) {
        await f.run(["--notices-only"]);
        assert.equal(await readFile(path.join(f.root, "packaging/llama.cpp/bin/NOTICES/llama.cpp-LICENSE"), "utf8"), "Fixture llama.cpp license\n");
      } else {
        await assert.rejects(f.run(["--notices-only"]), error => /not the pinned/.test(error.stderr));
        assert.equal(await readFile(path.join(f.root, "packaging/llama.cpp/bin/NOTICES/llama.cpp-LICENSE"), "utf8"), "stale notice\n");
      }
      assert.equal(await readFile(path.join(f.root, "packaging/llama.cpp/bin/COMMIT"), "utf8"), existing, "Refreshing notices preserves actual compilation provenance");
      await assert.rejects(readFile(path.join(f.root, "tools/fixture-bin/cmake.jsonl")), { code: "ENOENT" });
      await f.unchanged();
    } finally { await f.dispose(); }
  });
}
