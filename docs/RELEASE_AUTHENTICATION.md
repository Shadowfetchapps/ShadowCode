# Publisher authentication contract

The helpers in `scripts/native-release-auth.mjs` and `scripts/verify-native-release.sh` verify offline publisher signatures. They do not install, execute, download or publish an application, update trusted keys, or generate production keys. CI tests their behavior with disposable fixture keys; production trust is deliberately absent. The current installer and its explicit `--unverified` behavior are unchanged.

The Bash verifier loads `scripts/native-release-auth-lib.sh` from its own script directory. Keep both reviewed code files together in the trusted tooling bundle; do not obtain the library from the candidate download or source metadata as shell code. The shared parser and signature routines also support the separately reviewed installer prototype's historical receipts. Sharing these routines does not integrate the production installer or make its accepted-release state durable.

## Trust and security contract

The authorized canonical publisher is `Shadowfetchapps/ShadowCode`, repository ID `1377099349`, owner ID `209457103`. A read-only GitHub API query resolved the historical `ShadowfetchLinux/ShadowCode` origin to that identity. Only stable `MAJOR.MINOR.PATCH`, `x86_64-unknown-linux-gnu` releases are supported by this slice.

SHA-256 establishes byte integrity. The detached Ed25519 signature establishes that the holder of an already trusted publisher key signed the release envelope. It does not independently prove which workflow ran, that code is safe, or that the release is the newest available. The workflow identity is currently a publisher assertion; a future GitHub attestation can add an OIDC-backed workflow identity proof.

The trust directory must come from an independently trusted installer/policy bundle, not from the candidate download. There is no adjacent-key discovery or default test key. Explicitly pointing `--trust-dir` at attacker-provided material defeats the premise; the future production installer must choose its own pinned trust path, not accept one from release metadata or an environment override.

The initial installer/key bootstrap remains an owner decision: use a reviewed canonical repository revision and independently published public-key fingerprint, or another authenticated distribution channel. A verifier/key distributed beside an arbitrary download does not authenticate itself. This prototype is not a new automatic updater.

## Exact envelope and bindings

`RELEASE-AUTH` is at most 4096 bytes, printable ASCII plus LF/TAB, with a final LF and exactly 16 ordered lines. Unknown, missing, duplicate and reordered fields are rejected. Scalar values cannot contain `=` or TAB. Numeric version components and key epochs are bounded to nine decimal digits with no unnecessary leading zeroes. Commit is exactly 40 lowercase hexadecimal digits; hashes are exactly 64.

The line order is:

1. Literal `ShadowCode-Release-Auth-v1`.
2. `repository=Shadowfetchapps/ShadowCode`.
3. `repository-id=1377099349`.
4. `owner-id=209457103`.
5. `version=MAJOR.MINOR.PATCH`.
6. `tag=vMAJOR.MINOR.PATCH`, exactly matching version.
7. `commit=COMMIT_SHA`.
8. `target=x86_64-unknown-linux-gnu`.
9. `channel=stable`.
10. `key-id=SHA256_OF_ED25519_SPKI_DER`.
11. `key-epoch=POSITIVE_INTEGER`.
12. `manifest-sha256=SHA256_OF_EXACT_RELEASE_MANIFEST_JSON_BYTES`.
13. `checksums-sha256=SHA256_OF_EXACT_SHA256SUMS_BYTES`.
14. `asset=appimage<TAB>ShadowCode_VERSION_amd64.AppImage<TAB>SIZE<TAB>SHA256`.
15. `asset=deb<TAB>ShadowCode_VERSION_amd64.deb<TAB>SIZE<TAB>SHA256`.
16. `asset=runtime-sources<TAB>ShadowCode_VERSION_appimage-runtime-sources.tar.gz<TAB>SIZE<TAB>SHA256`.

`RELEASE-AUTH.sig` is exactly the 64-byte raw Ed25519 signature over all envelope bytes. There is no home-grown cryptography, prehash signature variant, URL field, arbitrary asset path or shell-sourced content. The domain marker prevents reuse as a different application statement.

The complete three-asset list is always authenticated. `SHA256SUMS` must contain exactly those entries in that order, matching the current `check-native-package.mjs` producer: each `HASH`, two spaces, basename, LF. Duplicate, extra, path-bearing and contradictory entries fail even when the checksum-file digest is correctly signed. Each artifact is nonempty and at most 32 GiB. The exact JSON manifest is hash-bound and limited to 1 MiB. Its audit contents are opaque to the shell verifier; the envelope is the authoritative install identity. The Node construction API additionally checks the manifest's current schema/tag/commit/target and exact asset map and requires the existing pretty-printed JSON serialization, which rejects duplicate JSON fields. Publisher receipt validation remains the responsibility of the existing release gates and publisher before signing.

Consumers may download only their selected package plus the envelope, signature, manifest and checksums; they do not need both package formats or the source archive. The selected basename must exactly match one authenticated entry.

## Trust policy and rotation

The explicit trust directory contains `policy` and one `KEY_ID.pem` public key per accepted/history key. PEM public keys are bounded to 1024 bytes and must be the canonical public SPKI serialization produced by Node/OpenSSL, with no extra PEM blocks. Both verifiers reject a matching private-key container, including a private key appended after a valid public block; only the signing API accepts private keys. The decoded key must be Ed25519, and the SHA-256 of its DER bytes must match the authenticated key ID. The shell additionally checks the exact Ed25519 SPKI encoding.

`policy` is at most 4096 bytes with these nine fixed lines, followed by one to eight unique key lines:

1. `ShadowCode-Release-Trust-v1`.
2. `repository=Shadowfetchapps/ShadowCode`.
3. `repository-id=1377099349`.
4. `owner-id=209457103`.
5. `target=x86_64-unknown-linux-gnu`.
6. `channel=stable`.
7. `minimum-epoch=POSITIVE_INTEGER`.
8. `minimum-version=MAJOR.MINOR.PATCH`.
9. `keys=ed25519-spki-sha256`.
10. Each `key=EPOCH<TAB>KEY_ID<TAB>MINIMUM_VERSION<TAB>MAXIMUM_VERSION`.

Each key has an inclusive version range and exactly one epoch. Unknown keys, different public bytes under a known ID, retired epochs and versions outside the trusted range/floor fail. No policy/key update can be authorized merely by placing new keys beside a release.

For a reviewed rotation, distribute a new authenticated trust bundle with the new public key and higher minimum epoch/version. Retain the old public key with its historical version range only when needed to verify an existing accepted-state receipt. The internal history check may ignore current minimum floors, but it still verifies the old signature, identity, key epoch and historical key range. This exception is not exposed as an install-verification API. Deleting an old key outright can make old state unverifiable; the installer must then fail pending explicit migration, not erase the state or assume a fresh install.

Production private-key generation/custody, public fingerprint distribution, key ranges, revocation and protected GitHub environment configuration remain owner decisions. Fixture keys are generated only in temporary test directories and deleted afterward. No production trust directory or key is supplied.

## Downgrade, replay and accepted state

Both verifiers can compare the candidate with a supplied prior accepted `RELEASE-AUTH` plus signature. They cryptographically verify that prior receipt, then reject a lower version, lower signing epoch, or any different envelope bytes under the same version. Identical retries are accepted. Comparison is numeric by stable version component, not lexicographic or a GitHub run number.

This module does **not** discover, persist or atomically advance the installed high-water mark. That belongs to later installer integration under the existing flock and journal. The installer must supply its actual durable highest accepted receipt, refuse missing/corrupt state for an existing authenticated installation, and never decrement it during rollback/recovery. Supplying an older genuinely signed receipt or deleting local state cannot be detected by a stateless verifier. Local same-user compromise is outside this trust boundary.

The trust policy's minimum version limits first-install replay to a known floor. A fresh offline host still cannot know the latest release or learn that a signing key was revoked after its last trusted policy update. There is no timestamp/expiry freshness claim. Unattended update discovery, trusted-clock freshness rules and database downgrade behavior are not implemented.

## APIs and exact staged bytes

`native-release-auth.mjs` exports:

- `createEnvelope({bundleDir, version, commit, keyId, keyEpoch}) -> Buffer`: validates all three actual package files, canonical checksums and current manifest bindings; emits deterministic envelope bytes.
- `signEnvelope(bytes, privateKeyPem) -> Buffer`: checks the envelope and signing-key fingerprint, then uses Node's Ed25519 implementation. It never creates a key or writes one.
- `verifyEnvelope({bytes, signature, trustDir}) -> metadata`: validates current trust policy and the Ed25519 signature. This verifies metadata only, not package contents.
- `verifyBundle({bundleDir, trustDir, artifact, previousDir?, expectedVersion?, expectedCommit?}) -> metadata`: verifies signature, previous-state constraints, exact metadata/checksum bindings and selected package bytes. This read-only Node API is for CI; it does not retain an execution-safe snapshot.
- Strict parser/identity helpers and explicit limits used by fixtures.

The end-user path is Bash plus OpenSSL 3 and ordinary GNU coreutils/diffutils; it requires no Node, Python, Rust, GitHub CLI or compiler:

```text
bash verify-native-release.sh \
  --bundle-dir DOWNLOADED_FILES \
  --trust-dir INDEPENDENTLY_TRUSTED_KEYS \
  --artifact ShadowCode_0.32.0_amd64.AppImage \
  --stage-dir NEW_PRIVATE_STAGE \
  --expect-version 0.32.0 \
  --expect-commit EXPECTED_COMMIT
```

`--previous-dir` optionally supplies the durable prior accepted envelope/signature. Every option is unique; missing mandatory arguments and unexpected options fail. There is no bypass/unsigned option.

The shell snapshots all relevant inputs into a private 0700 directory. GNU `dd` uses `nofollow,nonblock,count_bytes` and copies at most the observed, bounded file size plus one byte. This closes a post-check symlink swap and prevents an injected FIFO from blocking the input open/read. Exact snapshot size/hash/signature are checked. The selected artifact remains mode 0400 and is **never executed**. On success a same-filesystem, non-overwriting rename retains only the five verified files at the requested new stage path. Failure removes private scratch data and leaves any pre-existing stage untouched.

A future installer must execute/install only this verified private snapshot, never return to the original mutable download path. The read-only mode prevents accidental writes; it is not immutable against an attacker already controlling the same user. Existing checksum mismatch, managed-runtime and journal checks must remain. Existing `--unverified` behavior must not silently become a signature bypass.

## Production integration boundary

The helpers and their fixture suite are checked in and required by CI/release verification. Installer and publisher integration, production trust provisioning and actual production signing remain pending; passing this helper gate does not authenticate an existing release.

Later wiring should preserve every current CI gate and add authentication fixtures as a required gate. Split build/qualification from signing/publication. A fresh protected signing job should validate canonical repository IDs, tag event, exact version/tag/commit, current receipt set and downloaded immutable package bytes before signing. Production signing material must not be exposed to earlier build/test processes. The publisher must explicitly classify the new metadata/signature assets, stage and verify the full set without overwriting, and require local signature verification before publication.

The initial installer bundle should contain the verifier, approved public policy/keys and required icon; that removes the development-checkout prerequisite. Packaging/signing that bootstrap, approved trust provisioning, immutable signing-job identity, authenticated .deb invocation, durable accepted-state integration and live CI/release qualification are still outstanding. No current installed app or release is authenticated by this prototype alone.

## Evidence and limitations

Run `node --test scripts/test-native-release-auth.mjs` from the repository root. Tests use ephemeral Ed25519 keys, real OpenSSL verification, malicious-but-unexecuted executable fixtures, signed malformed metadata, wrong-key/identity/version/target cases, jointly changed package/checksums, history tampering, rotation and downgrade checks, bounds, path traversal, symlinks/FIFOs, and deterministic post-validation symlink/FIFO swaps through a test-only PATH wrapper. A restricted PATH fixture proves the shell verifier operates with only its declared runtime utilities. This is helper-level evidence, not a clean-host application install, native desktop qualification, production key ceremony or workflow execution.

References: [OpenSSL Ed25519 verification](https://docs.openssl.org/3.0/man1/openssl-pkeyutl/), [GitHub attestation identity verification](https://cli.github.com/manual/gh_attestation_verify), [offline attestation bundles and trusted roots](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/verify-attestations-offline), [GitHub protected environments](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments).
