# GLib 0.18.5: narrow RUSTSEC-2024-0429 backport

ShadowCode's GTK3/WebKit dependency graph requires GLib's Rust bindings at 0.18. The published 0.18.5 crate contains the immutable-out-pointer defect described in [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html). This directory preserves the actual package name/version and every published file, with only the two-line source correction from [gtk-rs commit b5a4071e439bef2b5eea76c3aa25e5ae84839e34](https://github.com/gtk-rs/gtk-rs-core/commit/b5a4071e439bef2b5eea76c3aa25e5ae84839e34).

Original archive: https://static.crates.io/crates/glib/glib-0.18.5.crate

Archive SHA256 (also the original Cargo.lock registry checksum): `233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5`.

The complete 267,679-byte archive was downloaded and its SHA256 matched before extraction. The adjacent `glib-0.18.5.provenance.json` records all 121 original file hashes, the published source revision, and the patched iterator hash. The MIT license remains unmodified at `glib-0.18.5/LICENSE`. No dependency versions, features or APIs were changed. The root Cargo patch selects this path for all transitive users; the crate is excluded from workspace membership.

Verification:

- `node scripts/native-glib-backport.mjs` checks the entire source inventory, refuses symlinks/unexpected files, and reverses the exact correction to verify that the original iterator bytes match their recorded hash.
- `node --test scripts/test-native-glib-backport.mjs` checks corruption/refusal and the path dependency's packaged MIT/provenance notice.
- `RUSTUP_TOOLCHAIN=1.95.0 node scripts/test-native-glib-variant.mjs` verifies the resolved source, builds only GLib and its dependencies with the release profile, and runs optimized iterator regressions against the exact emitted library. It needs the normal GLib development libraries. It does not build the desktop or use a GPU.
- `cargo tree --locked --offline -p shadowcode-desktop -i glib@0.18.5 --target x86_64-unknown-linux-gnu` displays the resolved path for the Linux desktop graph.

The optimized fixture checks every affected iterator operation, forward/backward and mixed iteration, exhaustion, overflow, empty arrays, empty strings, and Unicode. Native package/window qualification remains separate.

The advisory's published version range still includes 0.18.5. This is a source backport, not a claim that upstream released a corrected 0.18 crate or that a version-only scanner will understand the correction. There is no blanket advisory suppression or synthetic version bump. Retire this patch when a compatible, verified upstream release replaces it. Keep the source/provenance verification required until then.
