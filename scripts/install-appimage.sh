#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Install a ShadowCode AppImage for the current user.
#
#   * A provisioned publisher trust policy authenticates the release envelope,
#     manifest/checksums and exact AppImage bytes before any candidate executes.
#     Only its private verified snapshot is executed. --unverified cannot bypass
#     authentication; unsigned developer installation needs a separate path.
#   * Durable signed receipt generations retain the highest accepted release
#     even when runtime rollback restores an older working installation.
#   * Nothing is replaced until the new executable starts (--version) and the
#     llama.cpp runtime bundled inside it starts (llama-server --version with
#     LD_LIBRARY_PATH unset, printing the commit in its COMMIT file).
#   * The runtime is taken from the AppImage itself (usr/lib/shadowcode) and
#     installed into ~/.local/lib/shadowcode by rename; the previous runtime
#     stays as ~/.local/lib/shadowcode.previous until the install succeeds and
#     is restored if a later step fails or a handled signal interrupts install.
#   * Installers are serialized. Different bytes cannot replace an already
#     installed version. A recorded, unchanged pre-activation interruption is
#     recovered on retry or with --recover. An activation-started crash is also
#     recoverable while every recorded launcher/desktop integration is unchanged;
#     ambiguous or changed state is preserved for review.
#   * Settings and task history are never touched. Previous ShadowCode
#     AppImages are removed only after success.
set -euo pipefail
umask 077
usage() {
  echo 'Usage: install-appimage.sh /path/to/ShadowCode_VERSION_amd64.AppImage | --recover' >&2
  exit 2
}
fail() { echo "$*" >&2; exit 1; }
UNVERIFIED=0
RECOVER_ONLY=0
SOURCE_ARG=""
for arg in "$@"; do
  case "$arg" in
    --unverified) UNVERIFIED=1 ;;
    --recover) RECOVER_ONLY=1 ;;
    -*) usage ;;
    *) [[ -z "$SOURCE_ARG" ]] || usage; SOURCE_ARG="$arg" ;;
  esac
done
if [[ "$RECOVER_ONLY" == 1 ]]; then
  [[ -z "$SOURCE_ARG" && "$UNVERIFIED" == 0 ]] || usage
else
  [[ -n "$SOURCE_ARG" ]] || usage
fi

[[ "$UNVERIFIED" == 0 ]] || fail '--unverified is not supported by this signed-release installer. Publisher signatures are required; unsigned development installation needs a separate isolated development path.'

APPS="$HOME/Applications"
BIN="$HOME/.local/bin"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
LIB="$HOME/.local/lib/shadowcode"
JOURNAL="$(dirname "$LIB")/.shadowcode-install-intent"
# Resolve the actual script before selecting trusted sibling code and policy.
# A convenience symlink next to downloads must not choose a substitute bundle.
INSTALLER_FILE="$(realpath -- "${BASH_SOURCE[0]}")"
ROOT="$(cd "$(dirname "$INSTALLER_FILE")/.." && pwd)"
mkdir -p "$APPS" "$BIN" "$(dirname "$LIB")" "$DATA/applications" "$DATA/icons/hicolor/scalable/apps"

# Keep one stable lock inode, including between failed attempts. Two installers
# must not move or restore each other's runtime and AppImage.
LOCK="$(dirname "$LIB")/.shadowcode-install-lock"
[[ ! -L "$LOCK" ]] || fail 'Installer lock must not be a symlink.'
exec {INSTALL_LOCK}>"$LOCK"
flock -n "$INSTALL_LOCK" || fail 'Another ShadowCode install is already running.'

exists() { [[ -e "$1" || -L "$1" ]]; }
file_hash() { [[ -f "$1" && ! -L "$1" ]] && sha256sum < "$1" | awk '{print $1}'; }
# Fingerprint an optional integration file without following a leaf symlink.
# The mode is part of a regular file's identity; unknown file types fail closed.
integration_fingerprint() {
  if ! exists "$1"; then printf '%s\n' -
  elif [[ -L "$1" ]]; then
    { printf 'link\0'; readlink -z -- "$1"; } | sha256sum | awk '{print $1}'
  elif [[ -f "$1" ]]; then
    { printf 'file\0%s\0' "$(stat -c '%f' -- "$1")"; sha256sum < "$1"; } | sha256sum | awk '{print $1}'
  else return 1
  fi
}
# Bind names, entry types, modes, file bytes and symlink targets without
# following symlinks. Timestamps are deliberately excluded: renames and reads
# must not make the recorded runtime cease to match itself.
runtime_hash() {
  if [[ -L "$1" ]]; then
    { printf 'symlink\0'; readlink -z -- "$1"; } | sha256sum | awk '{print $1}'
  elif [[ -d "$1" ]]; then
    (
      cd -- "$1" || exit 1
      find . -print0 | LC_ALL=C sort -z |
      while IFS= read -r -d '' entry; do
        mode="$(stat -c '%f' -- "$entry")" || exit 1
        printf '%s\0%s\0' "$entry" "$mode" || exit 1
        if [[ -L "$entry" ]]; then readlink -z -- "$entry" || exit 1
        elif [[ -f "$entry" ]]; then sha256sum < "$entry" || exit 1
        elif [[ ! -d "$entry" ]]; then exit 1
        fi
      done
    ) | sha256sum | awk '{print $1}'
  else
    return 1
  fi
}
private_dir() { [[ -d "$1" && ! -L "$1" && "$(stat -c '%u:%a' -- "$1")" == "$(id -u):700" ]]; }
recovery_refusal() { fail "Interrupted install needs manual review: $*. Preserve $JOURNAL and $LIB.previous."; }
read_field() {
  local field="$JOURNAL/$1"
  [[ -f "$field" && ! -L "$field" && "$(stat -c '%u:%a:%h' -- "$field")" == "$(id -u):600:1" && "$(stat -c '%s' -- "$field")" -le 4096 ]] \
    || recovery_refusal 'invalid intent record'
  cat -- "$field"
}
# shellcheck source=install-release-state.sh
source "$ROOT/scripts/install-release-state.sh"

recover_install() {
  private_dir "$JOURNAL" || recovery_refusal 'intent directory is not private'
  local schema phase prior prior_hash had old_hash candidate candidate_hash existed new_hash stage_name stage live_hash previous_hash runtime active_link_candidate=0
  schema="$(read_field schema)"; phase="$(read_field phase)"
  [[ "$(read_field apps-root)" == "$(realpath -- "$APPS")" && "$(read_field library-root)" == "$(realpath -- "$(dirname "$LIB")")" ]] || recovery_refusal 'installation directory changed'
  prior="$(read_field prior-link)"; prior_hash="$(read_field prior-app-sha256)"
  had="$(read_field had-runtime)"; old_hash="$(read_field old-runtime-sha256)"
  candidate="$(read_field candidate)"; candidate_hash="$(read_field candidate-sha256)"
  existed="$(read_field candidate-existed)"; new_hash="$(read_field new-runtime-sha256)"
  stage_name="$(read_field stage)"
  [[ ( "$schema" == 1 || "$schema" == 2 || "$schema" == 3 ) && ( "$phase" == prepared || ( "$schema" == 3 && "$phase" == activation_started ) ) ]] || recovery_refusal 'unsupported intent or activation already started'
  if [[ "$schema" == 3 ]]; then
    [[ "$(read_field bin-root)" == "$(realpath -- "$BIN")" && "$(read_field applications-root)" == "$(realpath -- "$DATA/applications")" && "$(read_field icons-root)" == "$(realpath -- "$DATA/icons/hicolor/scalable/apps")" ]] || recovery_refusal 'launcher or desktop installation root changed'
  fi
  if [[ "$schema" == 1 ]] && exists "$STATE"; then recovery_refusal 'legacy intent conflicts with authenticated state'; fi
  [[ "$candidate" =~ ^ShadowCode-[0-9]+\.[0-9]+\.[0-9]+-x86_64\.AppImage$ && "$candidate_hash" =~ ^[a-f0-9]{64}$ && "$new_hash" =~ ^[a-f0-9]{64}$ ]] || recovery_refusal 'invalid candidate identity'
  [[ "$had" =~ ^[01]$ && "$existed" =~ ^[01]$ && "$stage_name" =~ ^\.shadowcode-install\.[A-Za-z0-9]{6}$ ]] || recovery_refusal 'invalid transaction identity'
  [[ ( "$had" == 1 && "$old_hash" =~ ^[a-f0-9]{64}$ ) || ( "$had" == 0 && "$old_hash" == - ) ]] || recovery_refusal 'invalid prior runtime identity'
  if [[ "$prior" == - ]]; then
    [[ "$prior_hash" == - ]] || recovery_refusal 'active AppImage changed'
  else
    [[ "$prior" =~ ^ShadowCode-[0-9]+\.[0-9]+\.[0-9]+-x86_64\.AppImage$ && "$prior_hash" =~ ^[a-f0-9]{64}$ ]] || recovery_refusal 'invalid prior AppImage identity'
    [[ "$(file_hash "$APPS/$prior")" == "$prior_hash" ]] || recovery_refusal 'prior AppImage changed'
  fi
  if exists "$APPS/ShadowCode.AppImage"; then
    [[ -L "$APPS/ShadowCode.AppImage" ]] || recovery_refusal 'active AppImage changed'
    case "$(readlink -- "$APPS/ShadowCode.AppImage")" in
      "$prior") [[ "$prior" != - ]] || recovery_refusal 'active AppImage changed' ;;
      "$candidate")
        [[ "$schema" == 3 && "$phase" == activation_started ]] || recovery_refusal 'active AppImage changed'
        active_link_candidate=1 ;;
      *) recovery_refusal 'active AppImage changed' ;;
    esac
  else
    [[ "$prior" == - ]] || recovery_refusal 'prior AppImage changed'
  fi
  stage="$(dirname "$LIB")/$stage_name"
  private_dir "$stage" || recovery_refusal 'staging directory changed or missing'
  if exists "$APPS/$candidate"; then
    [[ "$(file_hash "$APPS/$candidate")" == "$candidate_hash" ]] || recovery_refusal 'candidate AppImage changed'
  else
    [[ "$existed" == 0 ]] || recovery_refusal 'existing candidate AppImage disappeared'
  fi
  if exists "$APPS/$candidate.pending"; then
    [[ "$(file_hash "$APPS/$candidate.pending")" == "$candidate_hash" ]] || recovery_refusal 'pending AppImage is incomplete or changed'
  fi
  if exists "$APPS/.ShadowCode.AppImage.pending"; then
    [[ -L "$APPS/.ShadowCode.AppImage.pending" && "$(readlink -- "$APPS/.ShadowCode.AppImage.pending")" == "$candidate" ]] || recovery_refusal 'pending active AppImage link changed'
  fi
  for runtime in "$stage/squashfs-root/usr/lib/shadowcode" "$stage/recovery-runtime"; do
    if exists "$runtime"; then
      [[ "$(runtime_hash "$runtime")" == "$new_hash" ]] || recovery_refusal 'staged runtime changed'
    fi
  done
  live_hash=-; previous_hash=-
  if exists "$LIB"; then live_hash="$(runtime_hash "$LIB")" || recovery_refusal 'live runtime cannot be identified'; fi
  if exists "$LIB.previous"; then previous_hash="$(runtime_hash "$LIB.previous")" || recovery_refusal 'backup runtime cannot be identified'; fi
  if [[ "$had" == 1 ]]; then
    if [[ "$previous_hash" == "$old_hash" ]]; then
      [[ "$live_hash" == - || "$live_hash" == "$new_hash" ]] || recovery_refusal 'live runtime changed'
    else
      [[ "$previous_hash" == - && "$live_hash" == "$old_hash" ]] || recovery_refusal 'prior runtime changed or missing'
    fi
  else
    [[ "$previous_hash" == - && ( "$live_hash" == - || "$live_hash" == "$new_hash" ) ]] || recovery_refusal 'unexpected runtime or backup'
  fi
  if [[ "$schema" == 2 || "$schema" == 3 ]]; then
    validate_recovery_state "$stage" "$candidate" "$candidate_hash" "$existed" "$old_hash" "$live_hash" "$previous_hash"
  fi
  if [[ "$phase" == activation_started ]]; then
    local name location expected actual
    for name in shadow shadowcode icon desktop; do
      case "$name" in
        shadow) location="$BIN/shadow" ;;
        shadowcode) location="$BIN/shadowcode" ;;
        icon) location="$DATA/icons/hicolor/scalable/apps/shadow-agent.svg" ;;
        desktop) location="$DATA/applications/shadow-agent.desktop" ;;
      esac
      expected="$(read_field "integration-$name")"
      [[ "$expected" == - || "$expected" =~ ^[a-f0-9]{64}$ ]] || recovery_refusal 'invalid integration identity'
      actual="$(integration_fingerprint "$location")" || recovery_refusal "$name integration cannot be identified"
      [[ "$actual" == "$expected" ]] || recovery_refusal "$name integration changed after activation began"
    done
  fi
  if [[ "$active_link_candidate" == 1 ]]; then
    [[ "$(file_hash "$APPS/$candidate")" == "$candidate_hash" ]] || recovery_refusal 'active candidate AppImage changed'
  fi
  if exists "$stage/recovery-link"; then
    [[ "$active_link_candidate" == 1 && "$prior" != - && -L "$stage/recovery-link" && "$(readlink -- "$stage/recovery-link")" == "$prior" ]] || recovery_refusal 'recovery link changed'
  fi
  # All identities are checked before any recovery mutation. Preserve the
  # candidate until restoration succeeds, including if recovery is killed.
  if [[ "$active_link_candidate" == 1 ]]; then
    if [[ "$prior" == - ]]; then
      rm -f -- "$APPS/ShadowCode.AppImage" || recovery_refusal 'could not remove candidate active link'
    else
      if ! exists "$stage/recovery-link"; then ln -s -- "$prior" "$stage/recovery-link" || recovery_refusal 'could not stage prior active link'; fi
      mv -Tf -- "$stage/recovery-link" "$APPS/ShadowCode.AppImage" || recovery_refusal 'could not restore prior active link'
    fi
  fi
  if [[ "$live_hash" == "$new_hash" && ( "$had" == 0 || "$previous_hash" == "$old_hash" ) ]]; then
    ! exists "$stage/recovery-runtime" || recovery_refusal 'two candidate runtime copies'
    mv -T -- "$LIB" "$stage/recovery-runtime" || recovery_refusal 'could not retain candidate runtime'
  fi
  if [[ "$had" == 1 && "$previous_hash" == "$old_hash" ]]; then
    mv -T -- "$LIB.previous" "$LIB" || recovery_refusal 'could not restore prior runtime'
  fi
  [[ "$existed" == 1 ]] || rm -f -- "$APPS/$candidate"
  rm -f -- "$APPS/$candidate.pending"
  rm -f -- "$APPS/.ShadowCode.AppImage.pending"
  sync -f "$(dirname "$LIB")"
  # Retire the active record by rename, so interruption of recursive staging
  # cleanup cannot leave a partially deleted active journal blocking recovery.
  mv -T -- "$JOURNAL" "$stage/recovered-intent" || recovery_refusal 'could not retire completed recovery'
  rm -rf -- "$stage"
  printf 'Recovered the previous runtime and AppImage. Config and task history are preserved.\n'
}
if exists "$JOURNAL"; then recover_install; fi
[[ ! -e "$LIB.previous" && ! -L "$LIB.previous" ]] \
  || fail "A previous install was interrupted; preserve and recover $LIB.previous before retrying."
if [[ "$RECOVER_ONLY" == 1 ]]; then
  printf 'No interrupted install remains.\n'
  exit 0
fi

load_install_policy
load_accepted_receipt
check_legacy_identity
[[ -z "${SHADOWCODE_SHA256SUMS:-}" ]] || fail 'Separate checksum overrides cannot replace authenticated release metadata.'
DOWNLOAD="$(realpath -- "$SOURCE_ARG")"
[[ -f "$DOWNLOAD" && ! -L "$DOWNLOAD" ]] || fail 'AppImage not found.'
SOURCE_DIR="$(dirname "$DOWNLOAD")"
SOURCE_NAME="$(basename "$DOWNLOAD")"
[[ "$SOURCE_NAME" =~ ^ShadowCode_[0-9]+\.[0-9]+\.[0-9]+_amd64\.AppImage$ ]] || fail 'Only the authenticated AppImage role can be installed by this script.'
STAGE="$(mktemp -d "$(dirname "$LIB")/.shadowcode-install.XXXXXX")"
VERIFIED="$STAGE/verified-release"
DEST=''
JOURNAL_TEMP=''
JOURNAL_CREATED=0
RUNTIME_REPLACING=0
HAD_RUNTIME=0
[[ -e "$LIB" || -L "$LIB" ]] && HAD_RUNTIME=1
LINK_REPLACING=0
DEST_CREATING=0
INTEGRATION_ACTIVE=0
PREVIOUS_LINK="$(readlink "$APPS/ShadowCode.AppImage" 2>/dev/null || true)"
restore_integration() {
  local name=$1 destination=$2 prior=$3 candidate=$4 actual backup pending
  backup="$STAGE/integration-prior/$name"
  actual="$(integration_fingerprint "$destination")" || return 1
  [[ "$actual" == "$prior" || "$actual" == "$candidate" ]] || return 1
  if [[ "$prior" == - ]]; then
    ! exists "$backup" || return 1
  else
    [[ "$(integration_fingerprint "$backup")" == "$prior" ]] || return 1
  fi
  [[ "$actual" == "$prior" ]] && return 0
  if [[ "$prior" == - ]]; then
    rm -f -- "$destination"
  else
    pending="$destination.rollback.pending"
    ! exists "$pending" || return 1
    cp -a --no-dereference -- "$backup" "$pending" || return 1
    mv -Tf -- "$pending" "$destination"
  fi
}
finish() {
  local status=$?
  local restored=1
  trap - EXIT INT TERM
  if [[ "$status" != 0 ]]; then
    if [[ "$RUNTIME_REPLACING" == 1 ]]; then
      if [[ -e "$LIB.previous" || -L "$LIB.previous" ]]; then
        if rm -rf -- "$LIB"; then
          mv -T -- "$LIB.previous" "$LIB" || restored=0
        else
          restored=0
        fi
      elif [[ "$HAD_RUNTIME" == 0 ]]; then
        rm -rf -- "$LIB" || restored=0
      fi
    fi
    if [[ "$LINK_REPLACING" == 1 ]]; then
      if [[ -n "$PREVIOUS_LINK" ]]; then
        if ln -sfn -- "$PREVIOUS_LINK" "$APPS/.ShadowCode.AppImage.pending"; then
          mv -Tf -- "$APPS/.ShadowCode.AppImage.pending" "$APPS/ShadowCode.AppImage" || restored=0
        else
          restored=0
        fi
      else
        rm -f -- "$APPS/ShadowCode.AppImage" || restored=0
      fi
    fi
    if [[ "$INTEGRATION_ACTIVE" == 1 ]]; then
      restore_integration desktop "$DATA/applications/shadow-agent.desktop" "$DESKTOP_INTEGRATION" "$POST_DESKTOP_INTEGRATION" || restored=0
      restore_integration icon "$DATA/icons/hicolor/scalable/apps/shadow-agent.svg" "$ICON_INTEGRATION" "$POST_ICON_INTEGRATION" || restored=0
      restore_integration shadowcode "$BIN/shadowcode" "$SHADOWCODE_INTEGRATION" "$POST_SHADOWCODE_INTEGRATION" || restored=0
      restore_integration shadow "$BIN/shadow" "$SHADOW_INTEGRATION" "$POST_SHADOW_INTEGRATION" || restored=0
    fi
    if [[ "$DEST_CREATING" == 1 && "$restored" == 1 ]]; then
      rm -f -- "$DEST" || restored=0
    fi
    if [[ "$RUNTIME_REPLACING" == 1 || "$LINK_REPLACING" == 1 ]]; then
      if [[ "$restored" == 1 ]]; then
        echo 'Install failed; the previous runtime and AppImage were restored.' >&2
      else
        echo "Install failed and rollback needs attention; preserve $LIB.previous and $STAGE." >&2
      fi
    fi
  fi
  [[ -z "$DEST" ]] || rm -f -- "$DEST.pending"
  rm -f -- "$APPS/.ShadowCode.AppImage.pending"
  if [[ "$INTEGRATION_ACTIVE" == 1 && "$restored" == 1 ]]; then
    rm -f -- "$BIN/.shadow-install" "$BIN/.shadowcode-install" \
      "$DATA/icons/hicolor/scalable/apps/shadow-agent.svg.pending" \
      "$DATA/applications/shadow-agent.desktop.pending"
  fi
  if [[ "$STATE_ADVANCING" == 1 && "$STATE_DURABLE" == 0 ]]; then
    restored=0
    echo "Accepted-release state needs recovery; preserve $JOURNAL and $STAGE." >&2
  fi
  if [[ "$restored" == 1 ]]; then
    if [[ "$JOURNAL_CREATED" == 1 && -e "$JOURNAL" ]]; then
      mv -T -- "$JOURNAL" "$STAGE/completed-intent" || restored=0
    fi
    [[ "$restored" == 0 ]] || rm -rf -- "$STAGE"
  fi
  [[ -z "$JOURNAL_TEMP" ]] || rm -rf -- "$JOURNAL_TEMP"
  exit "$status"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM


VERIFY_ARGS=(--bundle-dir "$SOURCE_DIR" --trust-dir "$TRUST" --artifact "$SOURCE_NAME" --stage-dir "$VERIFIED")
[[ "$ACCEPTED_ID" == - ]] || VERIFY_ARGS+=(--previous-dir "$ACCEPTED_DIR")
bash "$ROOT/scripts/verify-native-release.sh" "${VERIFY_ARGS[@]}" || fail 'Publisher authentication failed; no downloaded candidate was executed.'
SOURCE="$VERIFIED/$SOURCE_NAME"
ACTUAL="$(file_hash "$SOURCE")" || fail 'Verified AppImage disappeared.'
VERSION="$(awk -F= '$1 == "version" {print $2}' "$VERIFIED/RELEASE-AUTH")"
VERIFIED_COMMIT="$(awk -F= '$1 == "commit" {print $2}' "$VERIFIED/RELEASE-AUTH")"
version_compare "$VERSION" "$FIRST_AUTH_VERSION"
(( CMP >= 0 )) || fail 'Signed candidate is before the first authenticated release boundary.'
chmod 500 "$SOURCE"
VERSION_LINE="$("$SOURCE" --appimage-extract-and-run --version)" || fail 'The AppImage did not start.'
[[ "$VERSION_LINE" == "ShadowCode $VERSION" ]] || fail 'The AppImage version differs from its signed release identity.'
DEST="$APPS/ShadowCode-${VERSION}-x86_64.AppImage"
[[ ! -e "$APPS/ShadowCode.AppImage" || -L "$APPS/ShadowCode.AppImage" ]] || fail 'ShadowCode.AppImage is not a symlink; refusing to replace it.'
DEST_EXISTED=0
if [[ -e "$DEST" || -L "$DEST" ]]; then
  [[ -f "$DEST" && ! -L "$DEST" ]] || fail 'The versioned AppImage is not a regular file.'
  cmp -s -- "$SOURCE" "$DEST" || fail "Different AppImage bytes are already installed as $VERSION; refusing to overwrite that version."
  DEST_EXISTED=1
fi
! exists "$APPS/.ShadowCode.AppImage.pending" || fail 'Active AppImage staging link already exists; preserve and review it.'

# The llama.cpp runtime shipped inside this AppImage.
(cd "$STAGE" && "$SOURCE" --appimage-extract usr/lib/shadowcode >/dev/null) \
  || fail 'Could not extract usr/lib/shadowcode from the AppImage.'
NEW_RUNTIME="$STAGE/squashfs-root/usr/lib/shadowcode"
[[ -x "$NEW_RUNTIME/llama-server" && -f "$NEW_RUNTIME/COMMIT" ]] \
  || fail 'This AppImage does not contain the managed llama.cpp runtime (usr/lib/shadowcode).'
[[ -s "$NEW_RUNTIME/architectures.txt" && -s "$NEW_RUNTIME/NOTICES/llama.cpp-LICENSE" ]] \
  || fail 'The bundled llama.cpp runtime is incomplete (architectures.txt or NOTICES missing).'
if [[ -n "$(find "$NEW_RUNTIME" -type l -lname '/*' -print -quit)" ]]; then
  fail 'The bundled llama.cpp runtime contains absolute symlinks; refusing to install it.'
fi
if [[ -n "$(find "$NEW_RUNTIME" -xtype l -print -quit)" ]]; then
  fail 'The bundled llama.cpp runtime contains dangling symlinks; refusing to install it.'
fi
RUNTIME_COMMIT="$(awk -F= '$1 == "commit" { print $2; exit }' "$NEW_RUNTIME/COMMIT")"
[[ "$RUNTIME_COMMIT" =~ ^[[:xdigit:]]{40}$ ]] || fail 'The bundled runtime COMMIT file has no commit.'
RUNTIME_VERSION="$(cd / && env -u LD_LIBRARY_PATH "$NEW_RUNTIME/llama-server" --version 2>&1)" \
  || fail "The bundled llama-server did not start: $RUNTIME_VERSION"
[[ "$RUNTIME_VERSION" == *"commit ${RUNTIME_COMMIT:0:7}"* ]] \
  || fail "The bundled llama-server does not report commit ${RUNTIME_COMMIT:0:7}."

# Persist exact recovery identities before the first destination mutation.
# These are data files, never shell-sourced commands or arbitrary paths.
PRIOR_HASH=-
if [[ -n "$PREVIOUS_LINK" ]]; then
  [[ "$PREVIOUS_LINK" =~ ^ShadowCode-[0-9]+\.[0-9]+\.[0-9]+-x86_64\.AppImage$ ]] \
    || fail 'Active AppImage link is not a versioned ShadowCode basename; preserve it and review before installing.'
  PRIOR_HASH="$(file_hash "$APPS/$PREVIOUS_LINK")" || fail 'Prior AppImage cannot be identified.'
fi
OLD_RUNTIME_HASH=-
[[ "$HAD_RUNTIME" == 0 ]] || OLD_RUNTIME_HASH="$(runtime_hash "$LIB")" || fail 'Prior runtime cannot be identified.'
NEW_RUNTIME_HASH="$(runtime_hash "$NEW_RUNTIME")" || fail 'Candidate runtime cannot be identified.'
SHADOW_INTEGRATION="$(integration_fingerprint "$BIN/shadow")" || fail 'Existing shadow launcher cannot be identified.'
SHADOWCODE_INTEGRATION="$(integration_fingerprint "$BIN/shadowcode")" || fail 'Existing shadowcode launcher cannot be identified.'
ICON_INTEGRATION="$(integration_fingerprint "$DATA/icons/hicolor/scalable/apps/shadow-agent.svg")" || fail 'Existing desktop icon cannot be identified.'
DESKTOP_INTEGRATION="$(integration_fingerprint "$DATA/applications/shadow-agent.desktop")" || fail 'Existing desktop entry cannot be identified.'
for pending in "$BIN/.shadow-install" "$BIN/.shadowcode-install" \
  "$DATA/icons/hicolor/scalable/apps/shadow-agent.svg.pending" \
  "$DATA/applications/shadow-agent.desktop.pending" \
  "$BIN/shadow.rollback.pending" "$BIN/shadowcode.rollback.pending" \
  "$DATA/icons/hicolor/scalable/apps/shadow-agent.svg.rollback.pending" \
  "$DATA/applications/shadow-agent.desktop.rollback.pending"; do
  ! exists "$pending" || fail "A desktop integration staging path already exists: $pending"
done
mkdir -m 700 "$STAGE/integration-prior" "$STAGE/integration-new"
stage_prior_integration() {
  local name=$1 source=$2 prior=$3
  if [[ "$prior" != - ]]; then
    cp -a --no-dereference -- "$source" "$STAGE/integration-prior/$name"
    [[ "$(integration_fingerprint "$STAGE/integration-prior/$name")" == "$prior" ]] \
      || fail "Existing $name integration changed during staging."
  fi
  [[ "$(integration_fingerprint "$source")" == "$prior" ]] \
    || fail "Existing $name integration changed during staging."
}
stage_prior_integration shadow "$BIN/shadow" "$SHADOW_INTEGRATION"
stage_prior_integration shadowcode "$BIN/shadowcode" "$SHADOWCODE_INTEGRATION"
stage_prior_integration icon "$DATA/icons/hicolor/scalable/apps/shadow-agent.svg" "$ICON_INTEGRATION"
stage_prior_integration desktop "$DATA/applications/shadow-agent.desktop" "$DESKTOP_INTEGRATION"
# Prepare exact candidate integration bytes before durable install intent or live writes.
cat > "$STAGE/integration-new/shadow" <<'LAUNCH'
#!/usr/bin/env bash
set -euo pipefail
exec "$HOME/Applications/ShadowCode.AppImage" --appimage-extract-and-run "$@"
LAUNCH
chmod 755 "$STAGE/integration-new/shadow"
ln -s shadow "$STAGE/integration-new/shadowcode"
cp -- "$ROOT/assets/icons/shadow-agent.svg" "$STAGE/integration-new/icon"
sed "s|^Exec=.*|Exec=\"${BIN}/shadow\" ui|; s|^TryExec=.*|TryExec=${BIN}/shadow|; s|^X-ShadowCode-Version=.*|X-ShadowCode-Version=${VERSION}|; /^X-ShadowCode-GitSha=/d" "$ROOT/packaging/shadow-agent.desktop" > "$STAGE/integration-new/desktop"
printf 'X-ShadowCode-GitSha=%s\n' "$VERIFIED_COMMIT" >> "$STAGE/integration-new/desktop"
POST_SHADOW_INTEGRATION="$(integration_fingerprint "$STAGE/integration-new/shadow")" || fail 'Candidate launcher cannot be identified.'
POST_SHADOWCODE_INTEGRATION="$(integration_fingerprint "$STAGE/integration-new/shadowcode")" || fail 'Candidate launcher alias cannot be identified.'
POST_ICON_INTEGRATION="$(integration_fingerprint "$STAGE/integration-new/icon")" || fail 'Candidate icon cannot be identified.'
POST_DESKTOP_INTEGRATION="$(integration_fingerprint "$STAGE/integration-new/desktop")" || fail 'Candidate desktop entry cannot be identified.'
sync -f "$STAGE"
[[ "$(file_hash "$SOURCE")" == "$ACTUAL" ]] || fail 'AppImage changed during validation.'
prepare_accepted_record
JOURNAL_TEMP="$(mktemp -d "$(dirname "$LIB")/.shadowcode-intent.XXXXXX")"
write_field() { printf '%s\n' "$2" > "$JOURNAL_TEMP/$1"; chmod 600 "$JOURNAL_TEMP/$1"; }
write_field schema 3
write_field phase prepared
write_field apps-root "$(realpath -- "$APPS")"
write_field library-root "$(realpath -- "$(dirname "$LIB")")"
write_field bin-root "$(realpath -- "$BIN")"
write_field applications-root "$(realpath -- "$DATA/applications")"
write_field icons-root "$(realpath -- "$DATA/icons/hicolor/scalable/apps")"
write_field integration-shadow "$SHADOW_INTEGRATION"
write_field integration-shadowcode "$SHADOWCODE_INTEGRATION"
write_field integration-icon "$ICON_INTEGRATION"
write_field integration-desktop "$DESKTOP_INTEGRATION"
write_field prior-link "${PREVIOUS_LINK:--}"
write_field prior-app-sha256 "$PRIOR_HASH"
write_field had-runtime "$HAD_RUNTIME"
write_field old-runtime-sha256 "$OLD_RUNTIME_HASH"
write_field candidate "$(basename "$DEST")"
write_field candidate-sha256 "$ACTUAL"
write_field candidate-existed "$DEST_EXISTED"
write_field new-runtime-sha256 "$NEW_RUNTIME_HASH"
write_field stage "$(basename "$STAGE")"
write_field accepted-prior "$STATE_PRIOR"
write_field accepted-candidate "$CANDIDATE_ID"
write_field accepted-record-sha256 "$CANDIDATE_RECORD_HASH"
write_field state-root "$STATE"
sync -f "$JOURNAL_TEMP"
JOURNAL_CREATED=1
mv -T -- "$JOURNAL_TEMP" "$JOURNAL"
JOURNAL_TEMP=""
sync -f "$(dirname "$LIB")"

advance_accepted_record

# Everything verified: replace.
if [[ "$DEST_EXISTED" == 0 ]]; then
  install -m 755 "$SOURCE" "$DEST.pending"
  [[ "$(file_hash "$DEST.pending")" == "$ACTUAL" ]] || fail 'Candidate destination copy changed.'
  DEST_CREATING=1
  mv -f "$DEST.pending" "$DEST"
fi
# Record intent before either rename: an error or handled signal can occur
# after the old runtime moved but before the candidate reaches its destination.
RUNTIME_REPLACING=1
[[ "$HAD_RUNTIME" == 0 ]] || mv -T -- "$LIB" "$LIB.previous"
mv -T -- "$NEW_RUNTIME" "$LIB"
# From this boundary on, launcher/desktop integration may have started. Do not
# infer a safe automatic rollback from runtime hashes alone after a crash.
printf 'activation_started\n' > "$JOURNAL/phase.pending"
chmod 600 "$JOURNAL/phase.pending"
mv -f -- "$JOURNAL/phase.pending" "$JOURNAL/phase"
sync -f "$JOURNAL"
LINK_REPLACING=1
ln -sfn "$(basename "$DEST")" "$APPS/.ShadowCode.AppImage.pending"
mv -Tf "$APPS/.ShadowCode.AppImage.pending" "$APPS/ShadowCode.AppImage"
INTEGRATION_ACTIVE=1
cp -a --no-dereference -- "$STAGE/integration-new/shadow" "$BIN/.shadow-install"
mv -Tf -- "$BIN/.shadow-install" "$BIN/shadow"
cp -a --no-dereference -- "$STAGE/integration-new/shadowcode" "$BIN/.shadowcode-install"
mv -Tf -- "$BIN/.shadowcode-install" "$BIN/shadowcode"
cp -a --no-dereference -- "$STAGE/integration-new/icon" "$DATA/icons/hicolor/scalable/apps/shadow-agent.svg.pending"
mv -Tf -- "$DATA/icons/hicolor/scalable/apps/shadow-agent.svg.pending" "$DATA/icons/hicolor/scalable/apps/shadow-agent.svg"
cp -a --no-dereference -- "$STAGE/integration-new/desktop" "$DATA/applications/shadow-agent.desktop.pending"
mv -Tf -- "$DATA/applications/shadow-agent.desktop.pending" "$DATA/applications/shadow-agent.desktop"
[[ "$(integration_fingerprint "$BIN/shadow")" == "$POST_SHADOW_INTEGRATION" && \
   "$(integration_fingerprint "$BIN/shadowcode")" == "$POST_SHADOWCODE_INTEGRATION" && \
   "$(integration_fingerprint "$DATA/icons/hicolor/scalable/apps/shadow-agent.svg")" == "$POST_ICON_INTEGRATION" && \
   "$(integration_fingerprint "$DATA/applications/shadow-agent.desktop")" == "$POST_DESKTOP_INTEGRATION" ]] \
  || fail 'Desktop integration changed during activation; rollback requires review if an unrelated edit occurred.'
# Success: drop the previous runtime and older AppImages.
mv -T -- "$JOURNAL" "$STAGE/completed-intent"
JOURNAL_CREATED=0
RUNTIME_REPLACING=0
LINK_REPLACING=0
DEST_CREATING=0
rm -rf -- "$LIB.previous"
for old in "$APPS"/ShadowCode-*-x86_64.AppImage; do
  [[ "$old" == "$DEST" || ! -f "$old" ]] || rm -- "$old"
done
command -v update-desktop-database >/dev/null && update-desktop-database "$DATA/applications" || true
printf 'Installed %s\nInstalled llama.cpp %s to %s\nConfig and task history are preserved. Restart the app to use the new release.\n' \
  "$DEST" "${RUNTIME_COMMIT:0:9}" "$LIB"
