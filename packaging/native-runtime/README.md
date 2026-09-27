# ShadowCode AppImage runtime

The native package uses AppImage's type2 runtime at the full commit in
`manifest.json`, with `isolated-extraction.patch` applied. It replaces the
content-addressed extraction directory with a private, mode-0700 `mkdtemp`
directory owned by each invocation. A short CLI launch or secondary desktop
activation can therefore finish without deleting another instance's resources.

The patch forwards SIGINT, SIGTERM, and SIGHUP to the payload, waits through
interrupted system calls, and preserves the payload's exit status. Inherited
ignored signals remain ignored, including `nohup`. It retains the child's PID
until forwarding is disabled, then reaps it. Ordinary exits clean the owned
extraction tree without following symlinks. The cleanup walker uses a positive
file-descriptor budget: musl's `nftw(..., 0, ...)` returns success without visiting
anything. Callback errors now propagate. The patch also bounds extraction paths
and fixes argv allocation for environment-based extraction.

SIGKILL and machine failure cannot run wrapper cleanup and may leave a temporary
directory. The native payload observes wrapper loss and stops owned work. The
explicit upstream `NO_CLEANUP` diagnostic option still retains extraction files.
FUSE-mounted operation uses the upstream mount path.

Patchset `isolated-extraction-v2` also rejects short writes and errors reported by
`fflush` or `fclose`. It closes each opened stream exactly once and returns through
the existing owned-directory cleanup before any payload can execute. This is
output-error handling, not a disk-performance or power-loss durability claim.

## Build and provenance

`node scripts/native-runtime.mjs` builds in Docker, or Podman with
`SHADOW_CONTAINER_ENGINE=podman`. The engine must already be usable by the build
user. The container is pinned by digest, every APK version is pinned in
`apk-packages.txt`, and source archives/patches have checksums. Unavailable pins
fail the build. Build tools stay in the container; none are copied into the app.
The runtime itself links statically, and the build rejects dynamic dependencies.

Source downloads use bounded attempts and verify the pinned digest before
publishing a file. A listed mirror is accepted only for identical bytes; zlib has
an Alpine distfiles fallback for upstream outages or unexpected response bodies.
Exhausted candidates fail the build, with no partial archive left behind.

`alpine-sources.json` records the exact Alpine recipes, patches, upstream archives,
and notices for musl, zlib, zstd, mimalloc, and GCC's runtime code. Libfuse and
squashfuse are built from the archives in `manifest.json`. The exported linker map
shows the actual static libraries used. The broader APK inventory describes the
build environment, not the contents of the application.

Outputs are cached under `target/native-runtime/<input hash>/`. Cache reuse checks
the runtime and retained artifact hashes. The application contains the notice
texts, patch, build recipe, inventories, compiler information, and a receipt.
Package verification compares the final runtime's `.text` section with the
source-built runtime, since AppImage tooling modifies metadata in other sections.

Packaging scripts apply `scripts/native-packaging-env.mjs` before invoking
container tools or linuxdeploy, so a dirty caller PATH cannot make linuxdeploy
stat `/usr/local/bin/node` → `/root/.hermes/...`.

`build-native.mjs` also emits `ShadowCode_<version>_appimage-runtime-sources.tar.gz`
next to the AppImage. It contains upstream sources, distribution patches, build
recipes, a linker map, debug symbols, and the matching receipt. Rebuild the runtime
from that archive by entering `sources/` and running:

```sh
docker build --platform linux/amd64 \
  --build-arg BASE_IMAGE="$(jq -r .container manifest.json)" \
  -t shadowcode-runtime-rebuild .
```

This uses the retained dependency sources in the archive without downloading
them again. Every retained archive and patch is rechecked; corruption fails the
build instead of triggering a replacement download. Fetching the pinned container
image and installing the pinned APK build tools still require network access.

The source artifact is separate from the installable application. Corresponding
sources for the other bundled system/application dependencies remain a separate
release gate. This recipe pins inputs; it does not claim byte-for-byte
reproducibility across compiler build timestamps.

## Regression checks

`node --test scripts/test-native-source-fetch.mjs` checks unexpected HTTP 200
bodies, HTTP errors, redirects, identical-byte mirror recovery, both digest
algorithms, retained-source reuse without HTTP requests, corrupt retained inputs,
and cleanup after exhausted downloads. It uses only a disposable local server.

After `node scripts/native-runtime.mjs` has populated the verified source cache,
`node --test scripts/test-native-runtime-write-errors.mjs` verifies the pinned
archive digest, applies the current patch with zero fuzz, and mechanically extracts
the actual C extraction/cleanup functions into a small host-compiler harness.
It uses a synthetic one-file SquashFS reader, real stdio, injected short/flush/close
errors, and real `/dev/full` refusal. Success actually executes a fixture payload;
errors must return false, never attempt payload execution, close once, remove the
owned directory and preserve a sibling sentinel. This fast test does not build
the full runtime or qualify the static musl/AppImage integration.

After packaging, `node scripts/test-native-runtime-sources.mjs` extracts the
shipped source archive, verifies its receipt, and rebuilds in a disposable
container with networking disabled. The already-built image supplies the pinned
toolchain; its previous source and output trees are cleared. The regression checks
that the new runtime has the same machine-code digest and writes its report to
`artifacts/native-package/source-rebuild/`. CI runs both source checks.

`node scripts/test-native-runtime.mjs [APPIMAGE]` launches simultaneous native
owners and eight short clients using one shared temporary directory. It verifies
private unique extraction, intact deferred WebKit resources, ordinary cleanup,
SIGINT/SIGTERM/SIGHUP forwarding, child-process cleanup, `nohup`, environment-mode
arguments, and bounded overlong paths. Its temporary profiles and processes are
owned by the test.

The packaged window test also runs with `SHADOW_NATIVE_DEFAULT_PROFILE=1` inside
`dbus-run-session`. It redirects XDG storage to a disposable profile, launches
three secondary instances, reloads the same window, and checks that only the
live window's extraction remains. Do not run that default-profile test on a
shared desktop DBus session.

When updating upstream or dependencies, review the patch against the new source,
refresh checksums/notices, inspect the linker map, and run the runtime, CLI,
window, and package checks. Do not substitute an unpinned continuous runtime.
