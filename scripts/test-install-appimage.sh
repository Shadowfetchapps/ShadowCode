#!/usr/bin/env bash
# Exercises scripts/install-appimage.sh with fake AppImages in an isolated HOME:
# checksum refusal, runtime installed from inside the AppImage with relative
# links, refusal of broken runtimes, rollback, and preserved profile data.
# Never touches the real HOME.
set -euo pipefail
SOURCE_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SCRATCH="$(mktemp -d)"
cleanup() { chmod -R u+w -- "$SCRATCH" 2>/dev/null || true; rm -rf -- "$SCRATCH"; }
trap cleanup EXIT
# Self-contained disposable trusted installer bundle; never provision the checkout.
ROOT="$SCRATCH/installer"
mkdir -p "$ROOT/scripts" "$ROOT/assets/icons" "$ROOT/packaging"
for script in install-appimage.sh install-release-state.sh native-release-auth-lib.sh verify-native-release.sh native-release-auth.mjs installer-auth-fixtures.mjs; do
  cp -- "$SOURCE_ROOT/scripts/$script" "$ROOT/scripts/$script"
done
cp -- "$SOURCE_ROOT/assets/icons/shadow-agent.svg" "$ROOT/assets/icons/shadow-agent.svg"
cp -- "$SOURCE_ROOT/packaging/shadow-agent.desktop" "$ROOT/packaging/shadow-agent.desktop"
INSTALLER="$ROOT/scripts/install-appimage.sh"
FIXTURE_TOOL="$ROOT/scripts/installer-auth-fixtures.mjs"
AUTH_FIXTURE_PRIVATE="$SCRATCH/fixture-private.pem"
node "$FIXTURE_TOOL" provision "$ROOT" "$AUTH_FIXTURE_PRIVATE"
# Exercise an existing built runtime in place as source data, copying into the
# isolated fake AppImage only for scenario7. No runtime download/build occurs.
REAL_BIN="$SOURCE_ROOT/packaging/llama.cpp/bin"
HOME="$SCRATCH/home"
export HOME
export XDG_DATA_HOME="$HOME/.local/share"
unset SHADOWCODE_SHA256SUMS SHADOWCODE_GIT_SHA
COMMIT_A="$(printf 'a1%.0s' {1..20})"
COMMIT_B="$(printf 'b2%.0s' {1..20})"
LIB="$HOME/.local/lib/shadowcode"
mkdir -p "$HOME/Applications" "$SCRATCH/release" "$XDG_DATA_HOME/shadow-agent" "$LIB"
printf 'keep this profile data' > "$XDG_DATA_HOME/shadow-agent/profile.txt"
printf 'old application' > "$HOME/Applications/ShadowCode-0.27.0-x86_64.AppImage"
ln -s ShadowCode-0.27.0-x86_64.AppImage "$HOME/Applications/ShadowCode.AppImage"
printf 'old runtime' > "$LIB/old-runtime-marker"

# fake_appimage FILE VERSION COMMIT MODE
# MODE: good | absolute-link | dangling-link | broken-server | wrong-commit | no-runtime
fake_appimage() {
  local file="$1" version="$2" commit="$3" mode="$4"
  mkdir -p "$(dirname "$file")"
  cat > "$file" <<FAKE
#!/usr/bin/env bash
set -euo pipefail
if [[ "\${1:-}" == "--appimage-extract-and-run" && "\${2:-}" == "--version" ]]; then
  printf 'ShadowCode $version\\n'
  exit 0
fi
if [[ "\${1:-}" == "--appimage-extract" && "\${2:-}" == "usr/lib/shadowcode" ]]; then
  [[ "$mode" == "no-runtime" ]] && exit 0
  dir=squashfs-root/usr/lib/shadowcode
  mkdir -p "\$dir/NOTICES"
  if [[ "$mode" == "real" ]]; then
    cp -a "$REAL_BIN/." "\$dir/"
    exit 0
  fi
  reported="${commit:0:9}"
  [[ "$mode" == "wrong-commit" ]] && reported="0000000ff"
  exit_code=0
  [[ "$mode" == "broken-server" ]] && exit_code=127
  printf '#!/bin/sh\\nif [ -n "\${LD_LIBRARY_PATH:-}" ]; then echo "LD_LIBRARY_PATH leaked" >&2; exit 3; fi\\necho "version: 0.4.1-dev (build 1, commit %s)" >&2\\nexit %s\\n' "\$reported" "\$exit_code" > "\$dir/llama-server"
  chmod 755 "\$dir/llama-server"
  printf 'lib' > "\$dir/libfake.so.0.1"
  ln -s libfake.so.0.1 "\$dir/libfake.so.0"
  [[ "$mode" == "absolute-link" ]] && ln -sfn "\$PWD/\$dir/libfake.so.0.1" "\$dir/libfake.so.0"
  [[ "$mode" == "dangling-link" ]] && ln -sfn libmissing.so.0 "\$dir/libfake.so.0"
  printf 'url=https://github.com/ggml-org/llama.cpp.git\\ncommit=$commit\\nbackend=vulkan+cpu\\n' > "\$dir/COMMIT"
  printf 'llama\\nqwen3\\n' > "\$dir/architectures.txt"
  printf 'MIT License\\n' > "\$dir/NOTICES/llama.cpp-LICENSE"
  exit 0
fi
exit 1
FAKE
  chmod +x "$file"
}
checksum() { node "$FIXTURE_TOOL" sign "$1" "$AUTH_FIXTURE_PRIVATE"; }
expect_refusal() {
  local label="$1"; shift
  if "$@" > "$SCRATCH/refused.txt" 2>&1; then
    echo "Installer accepted: $label" >&2
    cat "$SCRATCH/refused.txt" >&2
    exit 1
  fi
}
assert_state_unchanged() {
  test "$(readlink "$HOME/Applications/ShadowCode.AppImage")" = "$1"
  grep -Fxq "commit=$2" "$LIB/COMMIT"
  test ! -e "$LIB.previous"
  test "$(cat "$XDG_DATA_HOME/shadow-agent/profile.txt")" = 'keep this profile data'
  test -z "$(find "$HOME/.local/lib" -maxdepth 1 -name '.shadowcode-install.*' -print -quit)"
  test ! -e "$HOME/.local/lib/.shadowcode-install-intent"
}

APPIMAGE="$SCRATCH/release/0.28.0/ShadowCode_0.28.0_amd64.AppImage"
fake_appimage "$APPIMAGE" 0.28.0 "$COMMIT_A" good

# 1. Missing publisher metadata is refused; nothing changes.
expect_refusal 'missing SHA256SUMS' "$INSTALLER" "$APPIMAGE"
grep -Fq 'Publisher authentication failed' "$SCRATCH/refused.txt"
test "$(readlink "$HOME/Applications/ShadowCode.AppImage")" = ShadowCode-0.27.0-x86_64.AppImage
test -f "$LIB/old-runtime-marker"

# 2. Verified install: AppImage, runtime from inside it, launchers, entry.
checksum "$APPIMAGE"
"$INSTALLER" "$APPIMAGE" > "$SCRATCH/installed.txt"
grep -Fq 'Publisher signature verified' "$SCRATCH/installed.txt"
test -f "$HOME/Applications/ShadowCode-0.28.0-x86_64.AppImage"
test "$(readlink "$HOME/Applications/ShadowCode.AppImage")" = ShadowCode-0.28.0-x86_64.AppImage
test ! -e "$HOME/Applications/ShadowCode-0.27.0-x86_64.AppImage"
test ! -e "$LIB/old-runtime-marker"
grep -Fxq "commit=$COMMIT_A" "$LIB/COMMIT"
test "$(readlink "$LIB/libfake.so.0")" = libfake.so.0.1
test -s "$LIB/NOTICES/llama.cpp-LICENSE"
test -s "$LIB/architectures.txt"
env -u LD_LIBRARY_PATH "$LIB/llama-server" --version 2>&1 | grep -Fq "commit ${COMMIT_A:0:9}"
test ! -e "$LIB.previous"
test -z "$(find "$HOME/.local/lib" -maxdepth 1 -name '.shadowcode-install.*' -print -quit)"
test "$(cat "$XDG_DATA_HOME/shadow-agent/profile.txt")" = 'keep this profile data'
"$HOME/.local/bin/shadow" --version | grep -Fx 'ShadowCode 0.28.0'
test "$(readlink "$HOME/.local/bin/shadowcode")" = shadow
"$HOME/.local/bin/shadowcode" --version | grep -Fx 'ShadowCode 0.28.0'
DESKTOP="$XDG_DATA_HOME/applications/shadow-agent.desktop"
grep -Fxq 'X-ShadowCode-Version=0.28.0' "$DESKTOP"
grep -Fxq 'StartupWMClass=shadowcode' "$DESKTOP"
grep -Fxq "Exec=\"$HOME/.local/bin/shadow\" ui" "$DESKTOP"
test -f "$XDG_DATA_HOME/icons/hicolor/scalable/apps/shadow-agent.svg"
# The source commit comes from signed metadata; an environment override is ignored.
EXPECTED_SHA="$(printf 'c%.0s' {1..40})"
grep -Fxq "X-ShadowCode-GitSha=$EXPECTED_SHA" "$DESKTOP"
SHADOWCODE_GIT_SHA="$(printf 'a%.0s' {1..40})" "$INSTALLER" "$APPIMAGE" > /dev/null
grep -Fxq "X-ShadowCode-GitSha=$EXPECTED_SHA" "$DESKTOP"
test "$(grep -c '^X-ShadowCode-GitSha=' "$DESKTOP")" -eq 1
assert_state_unchanged ShadowCode-0.28.0-x86_64.AppImage "$COMMIT_A"

# 3. Broken runtimes inside a correctly checksummed AppImage are refused
#    before anything is replaced.
for mode in absolute-link dangling-link broken-server wrong-commit no-runtime; do
  BAD="$SCRATCH/release/0.28.1/ShadowCode_0.28.1_amd64.AppImage"
  rm -f "$BAD" "$SCRATCH/release/SHA256SUMS"
  fake_appimage "$BAD" 0.28.1 "$COMMIT_B" "$mode"
  checksum "$BAD"
  expect_refusal "runtime mode $mode" "$INSTALLER" "$BAD"
  case "$mode" in
    absolute-link) reason='contains absolute symlinks' ;;
    dangling-link) reason='contains dangling symlinks' ;;
    broken-server) reason='llama-server did not start' ;;
    wrong-commit) reason="does not report commit ${COMMIT_B:0:7}" ;;
    no-runtime) reason='does not contain the managed llama.cpp runtime' ;;
  esac
  grep -Fq "$reason" "$SCRATCH/refused.txt" || { cat "$SCRATCH/refused.txt" >&2; exit 1; }
  test ! -e "$HOME/Applications/ShadowCode-0.28.1-x86_64.AppImage"
  assert_state_unchanged ShadowCode-0.28.0-x86_64.AppImage "$COMMIT_A"
done

# 4. --unverified cannot bypass publisher authentication; a signed install follows.
UNVERIFIED="$SCRATCH/unverified/ShadowCode_0.28.1_amd64.AppImage"
mkdir -p "$(dirname "$UNVERIFIED")"
fake_appimage "$UNVERIFIED" 0.28.1 "$COMMIT_B" good
expect_refusal 'unverified cannot bypass signature' "$INSTALLER" --unverified "$UNVERIFIED"
grep -Fq 'Publisher signatures are required' "$SCRATCH/refused.txt"
assert_state_unchanged ShadowCode-0.28.0-x86_64.AppImage "$COMMIT_A"
checksum "$UNVERIFIED"
"$INSTALLER" "$UNVERIFIED" > /dev/null
assert_state_unchanged ShadowCode-0.28.1-x86_64.AppImage "$COMMIT_B"
test ! -e "$HOME/Applications/ShadowCode-0.28.0-x86_64.AppImage"

# 5. A failure after the swap restores the previous runtime and AppImage link.
NEXT="$SCRATCH/release/0.28.2/ShadowCode_0.28.2_amd64.AppImage"
rm -f "$SCRATCH/release/SHA256SUMS"
fake_appimage "$NEXT" 0.28.2 "$COMMIT_A" good
checksum "$NEXT"
cp -a -- "$HOME/.local/bin/shadow" "$SCRATCH/prior-shadow"
cp -a -- "$DESKTOP" "$SCRATCH/prior-desktop"
cp -a -- "$XDG_DATA_HOME/icons/hicolor/scalable/apps/shadow-agent.svg" "$SCRATCH/prior-icon"
chmod 555 "$XDG_DATA_HOME/applications"
expect_refusal 'unwritable desktop entry directory' "$INSTALLER" "$NEXT"
chmod 755 "$XDG_DATA_HOME/applications"
grep -Fq 'previous runtime and AppImage were restored' "$SCRATCH/refused.txt"
test ! -e "$HOME/Applications/ShadowCode-0.28.2-x86_64.AppImage"
assert_state_unchanged ShadowCode-0.28.1-x86_64.AppImage "$COMMIT_B"
cmp -s -- "$HOME/.local/bin/shadow" "$SCRATCH/prior-shadow"
cmp -s -- "$DESKTOP" "$SCRATCH/prior-desktop"
cmp -s -- "$XDG_DATA_HOME/icons/hicolor/scalable/apps/shadow-agent.svg" "$SCRATCH/prior-icon"
test "$(readlink "$HOME/.local/bin/shadowcode")" = shadow

# 6. A changed AppImage fails its checksum.
printf '# modified after checksum\n' >> "$NEXT"
expect_refusal 'checksum mismatch' "$INSTALLER" "$NEXT"
grep -Fq 'artifact digest/size mismatch' "$SCRATCH/refused.txt"
assert_state_unchanged ShadowCode-0.28.1-x86_64.AppImage "$COMMIT_B"

# 7. With a built runtime in this checkout, the real llama-server installs and
#    starts from the isolated ~/.local/lib/shadowcode (no GPU work: --version).
if [[ -x "$REAL_BIN/llama-server" && -f "$REAL_BIN/NOTICES/llama.cpp-LICENSE" ]]; then
  REAL_COMMIT="$(awk -F= '$1 == "commit" { print $2; exit }' "$REAL_BIN/COMMIT")"
  REAL="$SCRATCH/real/ShadowCode_0.28.3_amd64.AppImage"
  mkdir -p "$(dirname "$REAL")"
  fake_appimage "$REAL" 0.28.3 "$REAL_COMMIT" real
  checksum "$REAL"
  "$INSTALLER" "$REAL" > /dev/null
  assert_state_unchanged ShadowCode-0.28.3-x86_64.AppImage "$REAL_COMMIT"
  test -z "$(find "$LIB" -type l -lname '/*' -print -quit)"
  env -u LD_LIBRARY_PATH "$LIB/llama-server" --version 2>&1 | grep -Fq "commit ${REAL_COMMIT:0:9}"
  printf 'Real llama.cpp runtime %s installed and started from %s.\n' "${REAL_COMMIT:0:9}" "$LIB"
else
  printf 'SKIP installer-real-runtime: packaging/llama.cpp/bin is not built.\n'
fi

# 8. Failure between the two runtime renames must restore the live runtime.
# Command wrappers fail at actual filesystem boundaries; the installer has no
# test-only switches and all mutations still happen inside the isolated HOME.
PRIOR_LINK="$(readlink "$HOME/Applications/ShadowCode.AppImage")"
PRIOR_COMMIT="$(awk -F= '$1 == "commit" { print $2; exit }' "$LIB/COMMIT")"
FAULT_BIN="$SCRATCH/fault-bin"
mkdir -p "$FAULT_BIN"
REAL_MV="$(command -v mv)"
cat > "$FAULT_BIN/mv" <<'WRAPPER'
#!/usr/bin/env bash
set -euo pipefail
source_path="${@: -2:1}"
target_path="${@: -1}"
case "$SHADOW_TEST_FAULT" in
  old-runtime-failure)
    if [[ "$source_path" == "$HOME/.local/lib/shadowcode" ]]; then
      echo 'Injected old runtime rename failure' >&2
      exit 74
    fi ;;
  new-runtime-failure|rollback-failure)
    if [[ "$source_path" == */squashfs-root/usr/lib/shadowcode ]]; then
      echo 'Injected candidate runtime rename failure' >&2
      exit 74
    fi
    if [[ "$SHADOW_TEST_FAULT" == rollback-failure && "$source_path" == "$HOME/.local/lib/shadowcode.previous" ]]; then
      echo 'Injected rollback rename failure' >&2
      exit 75
    fi ;;
  after-old-runtime-term|after-new-runtime-term|after-old-runtime-kill|after-new-runtime-kill|after-restore-kill|after-activation-kill)
    if [[ ( "$SHADOW_TEST_FAULT" == after-old-runtime-* && "$target_path" == "$HOME/.local/lib/shadowcode.previous" ) ||
          ( "$SHADOW_TEST_FAULT" == after-new-runtime-* && "$source_path" == */squashfs-root/usr/lib/shadowcode ) ||
          ( "$SHADOW_TEST_FAULT" == after-restore-kill && "$source_path" == "$HOME/.local/lib/shadowcode.previous" ) ||
          ( "$SHADOW_TEST_FAULT" == after-activation-kill && "$source_path" == "$HOME/.local/lib/.shadowcode-install-intent/phase.pending" ) ]]; then
      "$SHADOW_TEST_REAL_MV" "$@"
      signal=TERM
      [[ "$SHADOW_TEST_FAULT" == *-kill ]] && signal=KILL
      echo "Injected $signal after runtime rename" >&2
      kill -"$signal" "$PPID"
      exit 0
    fi ;;
  restore-runtime-failure)
    if [[ "$source_path" == "$HOME/.local/lib/shadowcode.previous" ]]; then
      echo 'Injected recovery rename failure' >&2
      exit 76
    fi ;;
  after-link-kill)
    if [[ "$source_path" == "$HOME/Applications/.ShadowCode.AppImage.pending" && "$target_path" == "$HOME/Applications/ShadowCode.AppImage" ]]; then
      "$SHADOW_TEST_REAL_MV" "$@"
      echo 'Injected KILL after active link replacement' >&2
      kill -KILL "$PPID"
      exit 0
    fi ;;
  after-desktop-failure)
    if [[ "$source_path" == "$XDG_DATA_HOME/applications/shadow-agent.desktop.pending" && "$target_path" == "$XDG_DATA_HOME/applications/shadow-agent.desktop" ]]; then
      "$SHADOW_TEST_REAL_MV" "$@"
      echo 'Injected failure after desktop replacement' >&2
      exit 74
    fi ;;
esac
exec "$SHADOW_TEST_REAL_MV" "$@"
WRAPPER
chmod +x "$FAULT_BIN/mv"
NEXT="$SCRATCH/release/0.28.4/ShadowCode_0.28.4_amd64.AppImage"
rm -f "$SCRATCH/release/SHA256SUMS"
fake_appimage "$NEXT" 0.28.4 "$COMMIT_A" good
checksum "$NEXT"
cp -a -- "$HOME/.local/bin/shadow" "$SCRATCH/prior-shadow"
cp -a -- "$DESKTOP" "$SCRATCH/prior-desktop"
cp -a -- "$XDG_DATA_HOME/icons/hicolor/scalable/apps/shadow-agent.svg" "$SCRATCH/prior-icon"
for fault in old-runtime-failure new-runtime-failure after-old-runtime-term after-new-runtime-term after-desktop-failure; do
  expect_refusal "$fault" env PATH="$FAULT_BIN:$PATH" SHADOW_TEST_REAL_MV="$REAL_MV" SHADOW_TEST_FAULT="$fault" "$INSTALLER" "$NEXT"
  grep -Fq 'Injected' "$SCRATCH/refused.txt"
  grep -Fq 'previous runtime and AppImage were restored' "$SCRATCH/refused.txt"
  test -f "$LIB/COMMIT" || { echo 'Prior live runtime disappeared after failed replacement.' >&2; exit 1; }
  assert_state_unchanged "$PRIOR_LINK" "$PRIOR_COMMIT"
  test ! -e "$HOME/Applications/ShadowCode-0.28.4-x86_64.AppImage"
  test ! -e "$HOME/Applications/ShadowCode-0.28.4-x86_64.AppImage.pending"
  if [[ "$fault" == after-desktop-failure ]]; then
    cmp -s -- "$HOME/.local/bin/shadow" "$SCRATCH/prior-shadow"
    cmp -s -- "$DESKTOP" "$SCRATCH/prior-desktop"
    cmp -s -- "$XDG_DATA_HOME/icons/hicolor/scalable/apps/shadow-agent.svg" "$SCRATCH/prior-icon"
    test "$(readlink "$HOME/.local/bin/shadowcode")" = shadow
  fi
done

# 9. Same-version retries are byte-immutable, even with a valid new checksum.
PRIOR_VERSION=0.28.4
SAME_VERSION="$SCRATCH/same-version/ShadowCode_${PRIOR_VERSION}_amd64.AppImage"
fake_appimage "$SAME_VERSION" "$PRIOR_VERSION" "$COMMIT_A" good
printf '# changed bytes under the same version\n' >> "$SAME_VERSION"
checksum "$SAME_VERSION"
PRIOR_HASH="$(sha256sum "$HOME/Applications/$PRIOR_LINK")"
expect_refusal 'same-version changed bytes' "$INSTALLER" "$SAME_VERSION"
grep -Fq 'changed release under accepted version' "$SCRATCH/refused.txt"
test "$(sha256sum "$HOME/Applications/$PRIOR_LINK")" = "$PRIOR_HASH"
assert_state_unchanged "$PRIOR_LINK" "$PRIOR_COMMIT"

# 10. Another installer cannot enter the transaction while its lock is held.
exec {TEST_LOCK}>"$HOME/.local/lib/.shadowcode-install-lock"
flock -n "$TEST_LOCK"
expect_refusal 'concurrent installer' "$INSTALLER" "$NEXT"
grep -Fq 'Another ShadowCode install is already running' "$SCRATCH/refused.txt"
expect_refusal 'concurrent recovery' "$INSTALLER" --recover
grep -Fq 'Another ShadowCode install is already running' "$SCRATCH/refused.txt"
exec {TEST_LOCK}>&-
assert_state_unchanged "$PRIOR_LINK" "$PRIOR_COMMIT"

# 11. A failed rollback retains its recovery copy and durable intent; a later
# recovery can restore it without the original candidate or checksum file.
expect_refusal 'rollback rename failure' env PATH="$FAULT_BIN:$PATH" SHADOW_TEST_REAL_MV="$REAL_MV" SHADOW_TEST_FAULT=rollback-failure "$INSTALLER" "$NEXT"
grep -Fq 'rollback needs attention' "$SCRATCH/refused.txt"
grep -Fxq "commit=$PRIOR_COMMIT" "$LIB.previous/COMMIT"
test -n "$(find "$HOME/.local/lib" -maxdepth 1 -type d -name '.shadowcode-install.*' -print -quit)"
mv "$NEXT" "$NEXT.offline"
"$INSTALLER" --recover > "$SCRATCH/recovered.txt"
mv "$NEXT.offline" "$NEXT"
assert_state_unchanged "$PRIOR_LINK" "$PRIOR_COMMIT"
test "$(sha256sum "$HOME/Applications/$PRIOR_LINK")" = "$PRIOR_HASH"
test "$(cat "$XDG_DATA_HOME/shadow-agent/profile.txt")" = 'keep this profile data'

# 12. Actual SIGKILL between runtime renames is recovered from durable intent.
expect_refusal 'killed between runtime renames' env PATH="$FAULT_BIN:$PATH" SHADOW_TEST_REAL_MV="$REAL_MV" SHADOW_TEST_FAULT=after-old-runtime-kill "$INSTALLER" "$NEXT"
grep -Fq 'Injected KILL' "$SCRATCH/refused.txt"
grep -Fxq "commit=$PRIOR_COMMIT" "$LIB.previous/COMMIT"
test ! -e "$LIB"
test "$(sha256sum "$HOME/Applications/$PRIOR_LINK")" = "$PRIOR_HASH"
test "$(cat "$XDG_DATA_HOME/shadow-agent/profile.txt")" = 'keep this profile data'
"$INSTALLER" --recover > "$SCRATCH/recovered.txt"
assert_state_unchanged "$PRIOR_LINK" "$PRIOR_COMMIT"
grep -Fq 'Recovered the previous runtime and AppImage' "$SCRATCH/recovered.txt"
"$INSTALLER" --recover > "$SCRATCH/recovered-again.txt"
assert_state_unchanged "$PRIOR_LINK" "$PRIOR_COMMIT"

# 13. Recovery also survives its own failed or killed restore operation.
expect_refusal 'killed after candidate runtime rename' env PATH="$FAULT_BIN:$PATH" SHADOW_TEST_REAL_MV="$REAL_MV" SHADOW_TEST_FAULT=after-new-runtime-kill "$INSTALLER" "$NEXT"
expect_refusal 'recovery rename failure' env PATH="$FAULT_BIN:$PATH" SHADOW_TEST_REAL_MV="$REAL_MV" SHADOW_TEST_FAULT=restore-runtime-failure "$INSTALLER" --recover
grep -Fq 'could not restore prior runtime' "$SCRATCH/refused.txt"
test ! -e "$LIB"
grep -Fxq "commit=$PRIOR_COMMIT" "$LIB.previous/COMMIT"
expect_refusal 'killed during recovery' env PATH="$FAULT_BIN:$PATH" SHADOW_TEST_REAL_MV="$REAL_MV" SHADOW_TEST_FAULT=after-restore-kill "$INSTALLER" --recover
test -f "$LIB/COMMIT"
test ! -e "$LIB.previous"
"$INSTALLER" --recover > "$SCRATCH/recovered.txt"
assert_state_unchanged "$PRIOR_LINK" "$PRIOR_COMMIT"

# 14. A normal retry recovers before installing the requested candidate.
expect_refusal 'killed before retry' env PATH="$FAULT_BIN:$PATH" SHADOW_TEST_REAL_MV="$REAL_MV" SHADOW_TEST_FAULT=after-old-runtime-kill "$INSTALLER" "$NEXT"
"$INSTALLER" "$NEXT" > "$SCRATCH/retried.txt"
grep -Fq 'Recovered the previous runtime and AppImage' "$SCRATCH/retried.txt"
assert_state_unchanged ShadowCode-0.28.4-x86_64.AppImage "$COMMIT_A"
PRIOR_LINK=ShadowCode-0.28.4-x86_64.AppImage
PRIOR_COMMIT="$COMMIT_A"
# A same-version candidate existed before the transaction and must survive
# recovery even when the old and candidate runtime fingerprints are identical.
SAME_HASH="$(sha256sum "$HOME/Applications/$PRIOR_LINK")"
expect_refusal 'killed reinstall of identical version' env PATH="$FAULT_BIN:$PATH" SHADOW_TEST_REAL_MV="$REAL_MV" SHADOW_TEST_FAULT=after-new-runtime-kill "$INSTALLER" "$NEXT"
"$INSTALLER" --recover > /dev/null
assert_state_unchanged "$PRIOR_LINK" "$PRIOR_COMMIT"
test "$(sha256sum "$HOME/Applications/$PRIOR_LINK")" = "$SAME_HASH"
NEXT="$SCRATCH/release/0.28.5/ShadowCode_0.28.5_amd64.AppImage"
fake_appimage "$NEXT" 0.28.5 "$COMMIT_B" good
checksum "$NEXT"

# 15. Edited bindings are never treated as known transaction state. Nothing
# may be removed until every identity check has succeeded.
expect_refusal 'killed before identity checks' env PATH="$FAULT_BIN:$PATH" SHADOW_TEST_REAL_MV="$REAL_MV" SHADOW_TEST_FAULT=after-new-runtime-kill "$INSTALLER" "$NEXT"
JOURNAL="$HOME/.local/lib/.shadowcode-install-intent"
STAGED="$(find "$HOME/.local/lib" -maxdepth 1 -type d -name '.shadowcode-install.*' -print -quit)"
cp "$JOURNAL/stage" "$SCRATCH/stage-field"
printf '../../elsewhere\n' > "$JOURNAL/stage"
expect_refusal 'forged stage path' "$INSTALLER" --recover
grep -Fq 'invalid transaction identity' "$SCRATCH/refused.txt"
cp "$SCRATCH/stage-field" "$JOURNAL/stage"
cp "$JOURNAL/candidate" "$SCRATCH/candidate-field"
rm "$JOURNAL/candidate"
ln -s "$SCRATCH/candidate-field" "$JOURNAL/candidate"
expect_refusal 'symlink intent field' "$INSTALLER" --recover
grep -Fq 'invalid intent record' "$SCRATCH/refused.txt"
rm "$JOURNAL/candidate"
cp "$SCRATCH/candidate-field" "$JOURNAL/candidate"
chmod 600 "$JOURNAL/candidate"
cp "$JOURNAL/apps-root" "$SCRATCH/apps-root-field"
printf '/another/install\n' > "$JOURNAL/apps-root"
expect_refusal 'changed install root' "$INSTALLER" --recover
grep -Fq 'installation directory changed' "$SCRATCH/refused.txt"
cp "$SCRATCH/apps-root-field" "$JOURNAL/apps-root"
cp -p "$HOME/Applications/ShadowCode-0.28.5-x86_64.AppImage" "$SCRATCH/candidate-app"
printf 'external bytes\n' >> "$HOME/Applications/ShadowCode-0.28.5-x86_64.AppImage"
expect_refusal 'changed candidate AppImage' "$INSTALLER" --recover
grep -Fq 'candidate AppImage changed' "$SCRATCH/refused.txt"
mv "$SCRATCH/candidate-app" "$HOME/Applications/ShadowCode-0.28.5-x86_64.AppImage"
printf 'external change' > "$LIB.previous/external.txt"
expect_refusal 'edited old runtime' "$INSTALLER" --recover
grep -Fq 'prior runtime changed or missing' "$SCRATCH/refused.txt"
test -f "$LIB.previous/external.txt"
test -d "$JOURNAL" && test -d "$STAGED"
rm "$LIB.previous/external.txt"
ln -sfn other.AppImage "$HOME/Applications/ShadowCode.AppImage"
expect_refusal 'changed active link' "$INSTALLER" --recover
grep -Fq 'active AppImage changed' "$SCRATCH/refused.txt"
ln -sfn "$PRIOR_LINK" "$HOME/Applications/ShadowCode.AppImage"
printf 'external change' > "$LIB/external.txt"
expect_refusal 'edited candidate runtime' "$INSTALLER" --recover
grep -Fq 'live runtime changed' "$SCRATCH/refused.txt"
rm "$LIB/external.txt"
"$INSTALLER" --recover > /dev/null
assert_state_unchanged "$PRIOR_LINK" "$PRIOR_COMMIT"

# 16. A legacy backup has no trustworthy journal; preserve it for review.
mv "$LIB" "$LIB.previous"
expect_refusal 'legacy backup' "$INSTALLER" --recover
grep -Fq 'previous install was interrupted' "$SCRATCH/refused.txt"
grep -Fxq "commit=$PRIOR_COMMIT" "$LIB.previous/COMMIT"
mv "$LIB.previous" "$LIB"

# 17. A first-install interruption restores the recorded absence of runtime
# and active link, without deleting unrelated older version files.
mv "$LIB" "$SCRATCH/prior-runtime"
mv "$HOME/Applications/ShadowCode.AppImage" "$SCRATCH/prior-active-link"
expect_refusal 'killed first install' env PATH="$FAULT_BIN:$PATH" SHADOW_TEST_REAL_MV="$REAL_MV" SHADOW_TEST_FAULT=after-new-runtime-kill "$INSTALLER" "$NEXT"
"$INSTALLER" --recover > /dev/null
test ! -e "$LIB" && test ! -e "$LIB.previous"
test ! -e "$HOME/Applications/ShadowCode.AppImage"
test -f "$HOME/Applications/$PRIOR_LINK"
test ! -e "$HOME/Applications/ShadowCode-0.28.5-x86_64.AppImage"
mv "$SCRATCH/prior-runtime" "$LIB"
mv "$SCRATCH/prior-active-link" "$HOME/Applications/ShadowCode.AppImage"

# 18. A crash at activation's first durable marker is recoverable only while
# the active link and all four recorded launcher/desktop files are unchanged.
cp -p "$XDG_DATA_HOME/applications/shadow-agent.desktop" "$SCRATCH/prior-desktop"
expect_refusal 'killed at activation boundary' env PATH="$FAULT_BIN:$PATH" SHADOW_TEST_REAL_MV="$REAL_MV" SHADOW_TEST_FAULT=after-activation-kill "$INSTALLER" "$NEXT"
printf 'external edit\n' >> "$XDG_DATA_HOME/applications/shadow-agent.desktop"
expect_refusal 'changed desktop after activation' "$INSTALLER" --recover
grep -Fq 'desktop integration changed after activation began' "$SCRATCH/refused.txt"
test -d "$JOURNAL"
grep -Fxq "commit=$PRIOR_COMMIT" "$LIB.previous/COMMIT"
cp -p "$SCRATCH/prior-desktop" "$XDG_DATA_HOME/applications/shadow-agent.desktop"
"$INSTALLER" --recover > "$SCRATCH/recovered-activation.txt"
grep -Fq 'Recovered the previous runtime and AppImage' "$SCRATCH/recovered-activation.txt"
assert_state_unchanged "$PRIOR_LINK" "$PRIOR_COMMIT"

# 19. A crash after the active link changes recovers only if the package and
# all four desktop integrations still match their recorded identities.
expect_refusal 'killed after link replacement' env PATH="$FAULT_BIN:$PATH" SHADOW_TEST_REAL_MV="$REAL_MV" SHADOW_TEST_FAULT=after-link-kill "$INSTALLER" "$NEXT"
printf 'external edit\n' >> "$XDG_DATA_HOME/applications/shadow-agent.desktop"
expect_refusal 'changed desktop after link replacement' "$INSTALLER" --recover
grep -Fq 'desktop integration changed after activation began' "$SCRATCH/refused.txt"
test -d "$JOURNAL"
grep -Fxq "commit=$PRIOR_COMMIT" "$LIB.previous/COMMIT"
cp -p "$SCRATCH/prior-desktop" "$XDG_DATA_HOME/applications/shadow-agent.desktop"
"$INSTALLER" --recover > "$SCRATCH/recovered-after-link.txt"
grep -Fq 'Recovered the previous runtime and AppImage' "$SCRATCH/recovered-after-link.txt"
assert_state_unchanged "$PRIOR_LINK" "$PRIOR_COMMIT"
test "$(cat "$XDG_DATA_HOME/shadow-agent/profile.txt")" = 'keep this profile data'
printf 'AppImage installer checks passed.\n'
