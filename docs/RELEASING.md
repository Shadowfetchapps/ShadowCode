# Release procedure

The most recent documented release is [**1.0.0**](https://github.com/Shadowfetchapps/ShadowCode/releases/tag/v1.0.0), published on September 30, 2026 from `e0ab265` by [run 36677001772](https://github.com/Shadowfetchapps/ShadowCode/actions/runs/36677001772) (attempt 2) with all 15 gates, protected signing and publication passing. Attempt 1 stopped in the `release-tests` gate when the private test-session cleanup found a live, unreadable process of the runner's user on the hosted runner; the same test passed on the same commit in the `main` checks, so the unchanged run was re-run and the `v1.0.0` tag was not moved. Updating version files or
these instructions does not publish or sign a release. Bind final build, test
and authentication receipts to the exact candidate before publication.

Pushing a `v*` tag starts `.github/workflows/release.yml`. Publication follows
build/qualification, protected signing review and verification of all seven
public assets. The signing environment permits only the tag being released
(`v1.0.1` for 1.0.1); every later version needs reviewed trust-policy,
deployment-rule and tooling-pin updates. The workflow uses
`docs/RELEASE_NOTES.md` as the release text.

## 1. Version files

All four must carry the same version, and the tag must be `v<version>`.
`release.yml` checks each of them:

| File | Field |
| --- | --- |
| `Cargo.toml` | `[workspace.package] version` |
| `src-tauri/tauri.conf.json` | `version` |
| `ui/package.json` | `version` |
| `packaging/shadow-agent.desktop` | `X-ShadowCode-Version=` |

Update the workspace package entries in `Cargo.lock` and the root package
versions in `ui/package-lock.json` alongside those four files; keep locked
builds reproducible. Update `CHANGELOG.md` and `docs/RELEASE_NOTES.md`. The notes file becomes the
GitHub release text as it is, so it should start with this release. Notes of
earlier releases may follow below it, as they do now.

## 2. Managed llama.cpp runtime

The runtime is built from `tools/llama.cpp.pin` (`commit=` and
`spirv_headers_commit=`). On Ubuntu 24.04:

```sh
sudo apt-get install cmake ninja-build libssl-dev libvulkan-dev glslc spirv-headers
bash scripts/build-llama.cpp.sh --no-user-install
node --test scripts/test-llama-runtime.mjs
```

- **glslc.** Without it or the Vulkan headers, the script falls back to a
  CPU-only runtime with only a warning. Before packaging, check that
  `packaging/llama.cpp/bin/COMMIT` says `backend=vulkan+cpu`. The script looks
  for glslc in `SHADOWCODE_GLSLC`, then on `PATH`, then in a user-installed
  `org.freedesktop.Sdk` flatpak (`tools/glslc-flatpak.sh`).
- **cmake.** Found in `SHADOWCODE_CMAKE`, then `.venv/bin/cmake`, then on
  `PATH`.
- **Notices.** The script copies the license texts of llama.cpp/ggml,
  cpp-httplib, nlohmann/json, stb_image, miniaudio, subprocess.h and
  SPIRV-Headers into `NOTICES/`. The same texts are pinned with SHA-256 digests
  in [licenses/native](../licenses/native/README.md) (`llama.cpp-<short>/`,
  `SPIRV-Headers-<short>/`).
- **Moving the pin.** Update `tools/llama.cpp.pin` and those notice
  directories together. Run `--notices-only` to refresh `NOTICES/` and `COMMIT`
  of an existing build without recompiling. Packaging refuses a runtime whose
  commit or notices don't match the pin.

## 3. Local checks

```sh
npm --prefix ui ci
npm --prefix ui run typecheck
npm --prefix ui test
npm --prefix ui run build
(cd ui && npm run test:e2e)
cargo +1.95.0 fmt --all --check
cargo +1.95.0 clippy --workspace --all-targets --locked -- -D warnings
cargo +1.95.0 build -p shadowcode-desktop --locked
cargo +1.95.0 test --workspace --locked
node scripts/test-native-cli.mjs
node scripts/test-native-stress.mjs
node scripts/test-native-tui.mjs
node scripts/check-secrets.mjs
```

Build the desktop executable before running the Rust suite: the process tests
launch `target/debug/shadowcode`. `check-secrets.mjs` also runs in the CI
"Checks" workflow, not in `release.yml`; run it before pushing the tag.
Live turns against real accounts (`live_vendor_turn`) are described under
[Tests](../README.md#tests) in the README.

## 4. Packages

On the pinned Ubuntu 24.04 build environment, with Docker, or Podman plus
`SHADOW_CONTAINER_ENGINE=podman`, for the pinned AppImage runtime build:

```sh
node --test scripts/test-native-packaging-env.mjs
node scripts/build-native.mjs
node scripts/check-native-package.mjs \
  target/release/bundle/appimage/ShadowCode_VERSION_amd64.AppImage \
  target/release/bundle/deb/ShadowCode_VERSION_amd64.deb
node scripts/test-native-runtime-write-errors.mjs
node scripts/test-native-runtime-sources.mjs
node scripts/test-native-runtime.mjs
bash scripts/test-install-appimage.sh
```

- **`build-native.mjs`** checks `packaging/llama.cpp/bin` (pin, SPIRV-Headers
  commit, `architectures.txt`, notices) before it builds. It copies the runtime
  into the AppDir with its relative links kept, and repacks the deb with
  `/usr/lib/shadowcode`. It sets its own clean `PATH` and records the Git
  commit (`SHADOWCODE_COMMIT`, `-dirty` for local changes) for Settings ›
  About. The deb repack (`scripts/native-deb.mjs`) adds the Debian copyright
  and changelog (from the AppStream releases), the manual page and shell
  completions generated by the packaged executable, hicolor icons, lintian
  overrides and the `libc6` floor, strips the runtime and fixes modes.
- **`check-native-package.mjs`** checks both packages:
  - no absolute, escaping or dangling links;
  - `COMMIT` matches the pin;
  - every `NEEDED` library is bundled or an allowed system library;
  - `llama-server --version` prints the pinned commit and `--list-devices` runs
    with `LD_LIBRARY_PATH` unset;
  - notice digests match, and there are no Python or Node sidecars;
  - the deb depends on `libgomp1` and `libssl3`;
  - the deb is what distributions expect: root-owned 0755/0644 modes, no
    maintainer scripts or conffiles, copyright with the NOTICE, changelog,
    manual page and completions matching the executable, icons at every
    hicolor size, a stripped runtime and `libc6 (>= …)` covering every
    binary. When installed, `lintian` must report no errors
    (`SHADOWCODE_REQUIRE_LINTIAN=1` makes it mandatory) and
    `desktop-file-validate` must pass. `node --test scripts/test-native-deb.mjs`
    covers these checks. See [Distributing ShadowCode](DISTRIBUTING.md).
- **`test-install-appimage.sh`** uses fake AppImages in a temporary `HOME`. It
  covers checksum refusal, broken runtimes, `--unverified`, rollback after the
  runtime swap and a changed download. If `packaging/llama.cpp/bin` exists, it
  also runs the real `llama-server`.

## 5. Protected signing review and tag

The `v0.33.0` attempt at `9e385cb` failed before signing/publication in
[run 36414378697](https://github.com/Shadowfetchapps/ShadowCode/actions/runs/36414378697).
Do not move or overwrite that tag. The `v0.34.0` attempt at `3409d59` likewise
stopped in the `native-source` gate ([run 36499751141](https://github.com/Shadowfetchapps/ShadowCode/actions/runs/36499751141))
because two new opt-in network tests were not on the optional-test list, and
the `v0.34.1` attempt at `206fd03` stopped in `native-window` ([run 36507326107](https://github.com/Shadowfetchapps/ShadowCode/actions/runs/36507326107))
because the window test expected a ready model on the clean CI host; nothing
was signed and both tags stay unchanged. Version 0.33.1 was published from
`e15c4480e65db5650af012bb2a9773dbe89acf84`, with all 15 required build gates and
protected signing passing in [run 36427024374](https://github.com/Shadowfetchapps/ShadowCode/actions/runs/36427024374).
The workflow publication job failed on a transient draft-listing response;
publication resumed locally with the same frozen publisher and signed artifact, verifying
all seven remote asset files before publication. This does not make that workflow
run green. Existing 0.33.0 receipts do not qualify the new package bytes.
The first-authenticated install boundary remains 0.33.0; do not silently move
this durable boundary.

Public trust in `release/trust` currently allows epoch 1 and versions 0.33.0–1.0.1;
`release/install-policy` fixes the first authenticated version at 0.33.0.
The `release-signing` environment has the signing secret and requires review by
`Shadowfetchapps` (User ID `209457103`). Self-review is allowed and GitHub's
default administrator override remains unchanged. For 1.0.1 its sole
deployment rule is the tag `v1.0.1` (it was `v1.0.0` for 1.0.0, `v0.34.2` for
0.34.2 and `v0.33.1` for 0.33.1); `NATIVE_RELEASE_SIGNING_ENVIRONMENT` points
to that environment. For 1.0.1,
`NATIVE_RELEASE_TOOLING_COMMIT` pins the tagged release commit itself, as it
did for 0.33.1 (`e15c4480e65db5650af012bb2a9773dbe89acf84`).

Before tagging, finish review and push the tooling commit, then set
`NATIVE_RELEASE_TOOLING_COMMIT` to that exact 40-character SHA. Read back this pin from GitHub configuration before dispatching the release. Verify the environment protections and source/candidate
receipts before approving signing. Neither the public key nor configured secret
proves that a package has been built, signed or published. See the
[authentication contract](RELEASE_AUTHENTICATION.md) for bootstrap and custody.

```sh
git tag -a vVERSION -m "ShadowCode VERSION"
git push origin vVERSION
```

`release.yml` then:

1. Checks the tag against the version files.
2. Builds and tests the UI.
3. Runs Rust fmt, clippy and tests.
4. Exercises the CLI, stress suite, TUI, both MCP transports and the real
   WebKit window.
5. Builds the llama.cpp runtime, then both packages, and inspects them.
6. Rebuilds the AppImage runtime sources without network access.
7. Runs the packaged CLI, TUI, window and MCP checks.
8. Writes `SHA256SUMS` and runs the installer test.
9. Prepares an unsigned handoff with required receipts. After protected review,
   a fresh runner verifies that immutable handoff using pinned tooling and signs
   private snapshots; the build job never receives the key.
10. A separate publisher verifies and stages these seven assets in a draft,
    compares every remote asset's bytes, then publishes:
   - `ShadowCode_VERSION_amd64.AppImage`
   - `ShadowCode_VERSION_amd64.deb`
   - `ShadowCode_VERSION_appimage-runtime-sources.tar.gz`
   - `SHA256SUMS`
   - `RELEASE-MANIFEST.json`
   - `RELEASE-AUTH`
   - `RELEASE-AUTH.sig`

Identical published retries are read-only; changed bytes under an existing
version are refused. Do not overwrite published versioned assets.

Don't announce a tag until this job succeeds. Afterwards, compare the uploaded
checksums and the release text with what you expect.

## 6. Install check

Use the complete independently authenticated installer bundle, including its
`release/trust`, install policy, verifier helpers, icon and desktop entry. Keep
the downloaded AppImage beside `SHA256SUMS`, `RELEASE-MANIFEST.json`,
`RELEASE-AUTH` and `RELEASE-AUTH.sig`.

```sh
bash /path/to/trusted-bundle/scripts/install-appimage.sh /path/to/downloads/ShadowCode_0.33.1_amd64.AppImage
```

The installer accepts a single AppImage path or `--recover`; it does not accept
`--bundle-dir`, `--trust-dir` or `--expect-version`. Those belong to the separate
read-only verifier. No checksum-only or `--unverified` bypass is supported.
For manual Debian verification/install, see the [README](../README.md#debian-package).

Then open ShadowCode from the desktop entry and check:

- Settings, conversations and accounts are still there.
- **Settings › Local models › Runtime** shows the pinned commit and the
  `vulkan+cpu` backend.
- A local model loads.
