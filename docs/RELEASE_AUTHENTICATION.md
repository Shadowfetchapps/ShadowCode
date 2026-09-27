# Publisher authentication contract

The helpers in `scripts/native-release-auth.mjs` and `scripts/verify-native-release.sh` verify offline publisher signatures. They do not install, execute, download or publish an application, update trusted keys, or generate production keys. CI tests their behavior with disposable fixture keys; production trust is deliberately absent. The installer now uses these helpers before executing a candidate and explicitly refuses `--unverified`. Production public trust and a first signed release remain unprovisioned.

The Bash verifier loads `scripts/native-release-auth-lib.sh` from its own script directory. Keep both reviewed code files together in the trusted tooling bundle; do not obtain the library from the candidate download or source metadata as shell code. The installer uses the same reviewed routines for current candidates and signed historical receipts; it never sources release metadata as code.

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

The stateless verifier module does **not** discover or persist installed state. The installer now supplies its durable highest accepted receipt under the installation lock/journal, refuses missing/corrupt state for an existing authenticated installation, and never decrements it during rollback/recovery. Supplying an older genuinely signed receipt or deleting local state cannot be detected by a stateless verifier. Local same-user compromise is outside this trust boundary.

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

The installer executes and installs only this verified private snapshot, never the original mutable download path. The read-only mode prevents accidental writes; it is not immutable against an attacker already controlling the same user. Existing checksum mismatch, managed-runtime and journal checks must remain. The previous `--unverified` flag is explicitly refused before destination creation; it has not become a signature bypass.

## Release workflow and provisioning boundary

The release workflow now separates source build/qualification, signing, and publication. `prepare-native-release.mjs` creates an unsigned handoff containing the exact three packages, canonical checksums/manifest and required gate receipts. A fresh signing runner downloads that build's immutable Actions artifact ID as data. It checks out signing code and public trust from an independently reviewed commit, validates the current repository/tag/commit/run/attempt and all required receipts, and signs private snapshots. It does not execute packages, install source dependencies, or restore build caches. Only its signing step receives the private key.

A separate publisher runner receives the signing job's immutable artifact ID and repository write permission, but no signing secret. `publish-native-release.mjs` requires the exact seven public assets (three packages plus the four metadata/signature files), verifies private snapshots before any GitHub call, checks remote repository and tag identity, stages a draft, and compares every remote asset's bytes before publication. Identical published retries are read-only; changed bytes under an existing version are refused. `VERIFICATION.json` is bounded CI transport data, not a release asset. Run IDs qualify each attempt without changing immutable release bytes.

The workflow deliberately refuses missing owner configuration. Before enabling a new release, provision and review all of the following:

- An independently authenticated `release/trust` public policy and key bundle in the reviewed tooling commit; choose key custody, epoch/ranges/floors, fingerprint distribution and rotation/revocation policy. No production key or default fixture key is supplied.
- Repository variable `NATIVE_RELEASE_TOOLING_COMMIT`: the exact 40-character reviewed commit containing the current gate definitions, signing/publisher code, public trust and release-appropriate notes. Never infer this pin from an unreviewed tag.
- Repository variable `NATIVE_RELEASE_SIGNING_ENVIRONMENT`: an existing protected environment with reviewed required reviewers, self-review and allowed deployment-ref settings as supported. A workflow environment name alone does not establish these protections and can create an unprotected environment.
- Environment secret `NATIVE_RELEASE_SIGNING_KEY_PEM`, scoped only to that protected environment. Do not put the production key in repository-wide secrets or the build job.

Owner review of the release workflow/source and repository protections remains necessary: another authorized workflow could request the same environment secret. Local gate receipts are validated assertions, not independent attestations that an adversarial producer executed the commands. Workflow dependencies and exact artifact IDs bind the proposed job flow; local fixtures do not prove GitHub control-plane behavior. Remote checks are observations, not atomic compare-and-swap against independently authorized concurrent changes.

All existing required gates remain, with a new required `built-project-cleanup` gate running the exact release-profile cleanup test over actual installed UI dependencies and output. The default Rust suite transfers only this test to that gate; missing, failed or ignored qualification rejects signing and publication. This scope does not claim full Cargo-cache cleanup qualification.

The tracked AppImage installer now authenticates candidates and retains durable accepted-state receipts. Bootstrap distribution, authenticated .deb invocation, actual production signing, clean-host installation and full GitHub release qualification remain outstanding. Existing installed applications and public releases are not authenticated retroactively.

## Trusted installer and durable accepted state

Distribute a complete, independently authenticated bundle containing `scripts/install-appimage.sh`, `scripts/install-release-state.sh`, both Bash verifier files, `release/install-policy`, the `release/trust` policy/public keys, `assets/icons/shadow-agent.svg` and `packaging/shadow-agent.desktop`. A development checkout is not a runtime dependency when this bundle is complete. The installer resolves its actual script file before choosing sibling code/trust, so a convenience launcher symlink cannot choose an adjacent substitute bundle. The candidate, working directory and environment cannot override this trust root. Path resolution does not authenticate the initial bundle or protect it from its owner modifying it.

The fixed install policy has exactly these two lines, with the actual first signed stable version substituted by the release owner:

```text
ShadowCode-Install-Policy-v1
first-authenticated-version=MAJOR.MINOR.PATCH
```

This immutable boundary distinguishes old unsigned installations from an authenticated installation whose state disappeared. It is not the current minimum-version floor and must not be advanced during key rotation. Incoming candidates below it are refused before execution, even if the trust policy's floor is lower. No production boundary is supplied by the repository yet.

The installer accepts only the authenticated AppImage role. It checks the private snapshot's reported version and bundled runtime before changing installed files. Signed metadata is retained in private, content-addressed receipt generations under `~/.local/lib/.shadowcode-release-state`. A schema3 intent binds the prior/candidate receipt identities, exact app/runtime fingerprints, installation roots and pre-activation launcher/desktop identities. The accepted pointer becomes durable before app/runtime replacement; rollback can restore an earlier working application without lowering the highest accepted release.

Recovery accepts exact recorded pre-activation states. With a schema3 intent, it also recovers a crash at the activation marker only when the active AppImage still points to the prior version and the recorded launcher, symlink, icon and desktop-entry identities and their installation roots are unchanged. A changed integration or active link is preserved for manual review. During a handled installation failure, the installer restores launcher, symlink, icon and desktop-entry files from staged copies when the live identities still match the recorded prior or candidate files; an unrelated edit is preserved for review. A crash after these integration files change still requires manual recovery. Older schema1/schema2 intents retain their original recovery limits; schema2 activation-started intents are not promoted without the new evidence. Historical receipt checks retain signature/identity/key-range verification while allowing old current floors. First-window/database downgrade qualification remains incomplete. Restoring a complete old application/state backup cannot be detected by this offline local high-water record.

Compatibility change: the signed entry point rejects checksum-only installation, `SHADOWCODE_SHA256SUMS` overrides and `--unverified`. Use the documented source build with a separate profile for development; a separate unsigned packaged-development installer has not been implemented. Existing published unsigned assets must not be silently repackaged or overwritten under the same version.

## Evidence and limitations

Run `node --test scripts/test-native-release-auth.mjs` from the repository root. Tests use ephemeral Ed25519 keys, real OpenSSL verification, malicious-but-unexecuted executable fixtures, signed malformed metadata, wrong-key/identity/version/target cases, jointly changed package/checksums, history tampering, rotation and downgrade checks, bounds, path traversal, symlinks/FIFOs, and deterministic post-validation symlink/FIFO swaps through a test-only PATH wrapper. A restricted PATH fixture proves the shell verifier operates with only its declared runtime utilities. This is helper-level evidence, not a clean-host application install, native desktop qualification, production key ceremony or workflow execution.

References: [OpenSSL Ed25519 verification](https://docs.openssl.org/3.0/man1/openssl-pkeyutl/), [GitHub attestation identity verification](https://cli.github.com/manual/gh_attestation_verify), [offline attestation bundles and trusted roots](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/verify-attestations-offline), [GitHub protected environments](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments).
