#!/usr/bin/env bash
# Install a ShadowCode AppImage for the current user.
#
#   * The AppImage must match its entry in SHA256SUMS beside it (or in
#     SHADOWCODE_SHA256SUMS). Without a checksum file the install is refused
#     unless --unverified is given.
#   * Nothing is replaced until the new executable starts (--version) and the
#     llama.cpp runtime bundled inside it starts (llama-server --version with
#     LD_LIBRARY_PATH unset, printing the commit in its COMMIT file).
#   * The runtime is taken from the AppImage itself (usr/lib/shadowcode) and
#     installed into ~/.local/lib/shadowcode by rename; the previous runtime
#     stays as ~/.local/lib/shadowcode.previous until the install succeeds and
#     is restored if a later step fails or a handled signal interrupts install.
#   * Installers are serialized. Different bytes cannot replace an already
#     installed version. A recorded, unchanged pre-activation interruption is
#     recovered on retry or with --recover; ambiguous backups are preserved.
#   * Settings and task history are never touched. Previous ShadowCode
#     AppImages are removed only after success.
set -euo pipefail
usage() {
  echo 'Usage: install-appimage.sh [--unverified] /path/to/ShadowCode_VERSION_amd64.AppImage | --recover' >&2
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

APPS="$HOME/Applications"
BIN="$HOME/.local/bin"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
LIB="$HOME/.local/lib/shadowcode"
JOURNAL="$(dirname "$LIB")/.shadowcode-install-intent"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$APPS" "$BIN" "$(dirname "$LIB")" "$DATA/applications" "$DATA/icons/hicolor/scalable/apps"

# Keep one stable lock inode, including between failed attempts. Two installers
# must not move or restore each other's runtime and AppImage.
LOCK="$(dirname "$LIB")/.shadowcode-install-lock"
[[ ! -L "$LOCK" ]] || fail 'Installer lock must not be a symlink.'
exec {INSTALL_LOCK}>"$LOCK"
flock -n "$INSTALL_LOCK" || fail 'Another ShadowCode install is already running.'

exists() { [[ -e "$1" || -L "$1" ]]; }
file_hash() { [[ -f "$1" && ! -L "$1" ]] && sha256sum < "$1" | awk '{print $1}'; }
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
recover_install() {
  private_dir "$JOURNAL" || recovery_refusal 'intent directory is not private'
  local schema phase prior prior_hash had old_hash candidate candidate_hash existed new_hash stage_name stage live_hash previous_hash runtime
  schema="$(read_field schema)"; phase="$(read_field phase)"
  [[ "$(read_field apps-root)" == "$(realpath -- "$APPS")" && "$(read_field library-root)" == "$(realpath -- "$(dirname "$LIB")")" ]] || recovery_refusal 'installation directory changed'
  prior="$(read_field prior-link)"; prior_hash="$(read_field prior-app-sha256)"
  had="$(read_field had-runtime)"; old_hash="$(read_field old-runtime-sha256)"
  candidate="$(read_field candidate)"; candidate_hash="$(read_field candidate-sha256)"
  existed="$(read_field candidate-existed)"; new_hash="$(read_field new-runtime-sha256)"
  stage_name="$(read_field stage)"
  [[ "$schema" == 1 && "$phase" == prepared ]] || recovery_refusal 'unsupported intent or activation already started'
  [[ "$candidate" =~ ^ShadowCode-[0-9]+\.[0-9]+\.[0-9]+-x86_64\.AppImage$ && "$candidate_hash" =~ ^[a-f0-9]{64}$ && "$new_hash" =~ ^[a-f0-9]{64}$ ]] || recovery_refusal 'invalid candidate identity'
  [[ "$had" =~ ^[01]$ && "$existed" =~ ^[01]$ && "$stage_name" =~ ^\.shadowcode-install\.[A-Za-z0-9]{6}$ ]] || recovery_refusal 'invalid transaction identity'
  [[ ( "$had" == 1 && "$old_hash" =~ ^[a-f0-9]{64}$ ) || ( "$had" == 0 && "$old_hash" == - ) ]] || recovery_refusal 'invalid prior runtime identity'
  if [[ "$prior" == - ]]; then
    if [[ "$prior_hash" != - ]] || exists "$APPS/ShadowCode.AppImage"; then recovery_refusal 'active AppImage changed'; fi
  else
    [[ "$prior" =~ ^ShadowCode-[0-9]+\.[0-9]+\.[0-9]+-x86_64\.AppImage$ && "$prior_hash" =~ ^[a-f0-9]{64}$ ]] || recovery_refusal 'invalid prior AppImage identity'
    [[ -L "$APPS/ShadowCode.AppImage" && "$(readlink -- "$APPS/ShadowCode.AppImage")" == "$prior" && "$(file_hash "$APPS/$prior")" == "$prior_hash" ]] || recovery_refusal 'prior AppImage changed'
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
  # All identities are checked before any recovery mutation. Preserve the
  # candidate until restoration succeeds, including if recovery is killed.
  if [[ "$live_hash" == "$new_hash" && ( "$had" == 0 || "$previous_hash" == "$old_hash" ) ]]; then
    ! exists "$stage/recovery-runtime" || recovery_refusal 'two candidate runtime copies'
    mv -T -- "$LIB" "$stage/recovery-runtime" || recovery_refusal 'could not retain candidate runtime'
  fi
  if [[ "$had" == 1 && "$previous_hash" == "$old_hash" ]]; then
    mv -T -- "$LIB.previous" "$LIB" || recovery_refusal 'could not restore prior runtime'
  fi
  [[ "$existed" == 1 ]] || rm -f -- "$APPS/$candidate"
  rm -f -- "$APPS/$candidate.pending"
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

SOURCE="$(realpath -- "$SOURCE_ARG")"
[[ -f "$SOURCE" ]] || fail 'AppImage not found.'
SOURCE_DIR="$(dirname "$SOURCE")"
SOURCE_NAME="$(basename "$SOURCE")"
CHECKSUMS="${SHADOWCODE_SHA256SUMS:-$SOURCE_DIR/SHA256SUMS}"
ACTUAL="$(file_hash "$SOURCE")" || fail 'AppImage must be a regular file.'
if [[ -f "$CHECKSUMS" ]]; then
  EXPECTED="$(awk -v name="$SOURCE_NAME" '{ file=$2; sub(/^\*/, "", file); count=split(file, parts, "/"); if (parts[count] == name) { print $1; exit } }' "$CHECKSUMS")"
  [[ "$EXPECTED" =~ ^[[:xdigit:]]{64}$ ]] || fail "No SHA-256 entry for $SOURCE_NAME in $CHECKSUMS"
  [[ "$ACTUAL" == "$EXPECTED" ]] || fail "Checksum mismatch for $SOURCE_NAME; refusing to install it."
  printf 'Verified SHA-256 from %s\n' "$CHECKSUMS"
elif [[ "$UNVERIFIED" == 1 ]]; then
  printf 'No SHA256SUMS beside the AppImage; installing unverified as requested (--unverified).\n' >&2
else
  fail "No SHA256SUMS beside $SOURCE_NAME. Download SHA256SUMS from the same release, or pass --unverified to install without checking."
fi
chmod +x "$SOURCE"
VERSION_LINE="$("$SOURCE" --appimage-extract-and-run --version)" || fail 'The AppImage did not start.'
[[ "$VERSION_LINE" =~ ^ShadowCode\ ([0-9]+\.[0-9]+\.[0-9]+)$ ]] || fail 'Not a supported ShadowCode release.'
VERSION="${BASH_REMATCH[1]}"
DEST="$APPS/ShadowCode-${VERSION}-x86_64.AppImage"
[[ ! -e "$APPS/ShadowCode.AppImage" || -L "$APPS/ShadowCode.AppImage" ]] \
  || fail 'ShadowCode.AppImage is not a symlink; refusing to replace it.'
# A retry may reuse identical bytes, but must not destroy the only rollback
# copy by replacing a versioned path with different content.
DEST_EXISTED=0
if [[ -e "$DEST" || -L "$DEST" ]]; then
  [[ -f "$DEST" && ! -L "$DEST" ]] || fail 'The versioned AppImage is not a regular file.'
  cmp -s -- "$SOURCE" "$DEST" \
    || fail "Different AppImage bytes are already installed as $VERSION; refusing to overwrite that version."
  DEST_EXISTED=1
fi

# Stage next to the destination so the final step is a rename.
STAGE="$(mktemp -d "$(dirname "$LIB")/.shadowcode-install.XXXXXX")"
JOURNAL_TEMP=""
JOURNAL_CREATED=0
RUNTIME_REPLACING=0
HAD_RUNTIME=0
[[ -e "$LIB" || -L "$LIB" ]] && HAD_RUNTIME=1
LINK_REPLACING=0
DEST_CREATING=0
PREVIOUS_LINK="$(readlink "$APPS/ShadowCode.AppImage" 2>/dev/null || true)"
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
  rm -f -- "$DEST.pending" "$APPS/.ShadowCode.AppImage.pending"
  if [[ "$restored" == 1 ]]; then
    [[ "$JOURNAL_CREATED" == 0 ]] || rm -rf -- "$JOURNAL"
    rm -rf -- "$STAGE"
  fi
  [[ -z "$JOURNAL_TEMP" ]] || rm -rf -- "$JOURNAL_TEMP"
  exit "$status"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

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
[[ "$(file_hash "$SOURCE")" == "$ACTUAL" ]] || fail 'AppImage changed during validation.'
JOURNAL_TEMP="$(mktemp -d "$(dirname "$LIB")/.shadowcode-intent.XXXXXX")"
write_field() { printf '%s\n' "$2" > "$JOURNAL_TEMP/$1"; chmod 600 "$JOURNAL_TEMP/$1"; }
write_field schema 1
write_field phase prepared
write_field apps-root "$(realpath -- "$APPS")"
write_field library-root "$(realpath -- "$(dirname "$LIB")")"
write_field prior-link "${PREVIOUS_LINK:--}"
write_field prior-app-sha256 "$PRIOR_HASH"
write_field had-runtime "$HAD_RUNTIME"
write_field old-runtime-sha256 "$OLD_RUNTIME_HASH"
write_field candidate "$(basename "$DEST")"
write_field candidate-sha256 "$ACTUAL"
write_field candidate-existed "$DEST_EXISTED"
write_field new-runtime-sha256 "$NEW_RUNTIME_HASH"
write_field stage "$(basename "$STAGE")"
sync -f "$JOURNAL_TEMP"
JOURNAL_CREATED=1
mv -T -- "$JOURNAL_TEMP" "$JOURNAL"
JOURNAL_TEMP=""
sync -f "$(dirname "$LIB")"

# Everything verified: replace.
if [[ "$DEST_EXISTED" == 0 ]]; then
  install -m 755 "$SOURCE" "$DEST.pending"
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
# Desktop launch uses extraction mode so libfuse2 is not required.
cat > "$BIN/.shadow-install" <<'LAUNCH'
#!/usr/bin/env bash
set -euo pipefail
exec "$HOME/Applications/ShadowCode.AppImage" --appimage-extract-and-run "$@"
LAUNCH
chmod +x "$BIN/.shadow-install"
mv -f "$BIN/.shadow-install" "$BIN/shadow"
ln -sfn shadow "$BIN/.shadowcode-install"
mv -Tf "$BIN/.shadowcode-install" "$BIN/shadowcode"
cp "$ROOT/assets/icons/shadow-agent.svg" "$DATA/icons/hicolor/scalable/apps/shadow-agent.svg"
sed "s|^Exec=.*|Exec=\"${BIN}/shadow\" ui|; s|^TryExec=.*|TryExec=${BIN}/shadow|; s|^X-ShadowCode-Version=.*|X-ShadowCode-Version=${VERSION}|; /^X-ShadowCode-GitSha=/d" "$ROOT/packaging/shadow-agent.desktop" > "$DATA/applications/shadow-agent.desktop.pending"
# Record the source commit when known: SHADOWCODE_GIT_SHA wins, then the
# checkout this script runs from. Omitted rather than guessed otherwise.
GIT_SHA="${SHADOWCODE_GIT_SHA:-$(git -C "$ROOT" rev-parse --verify HEAD 2>/dev/null || true)}"
if [[ "$GIT_SHA" =~ ^[[:xdigit:]]{40}$ ]]; then
  printf 'X-ShadowCode-GitSha=%s\n' "$GIT_SHA" >> "$DATA/applications/shadow-agent.desktop.pending"
fi
mv -f "$DATA/applications/shadow-agent.desktop.pending" "$DATA/applications/shadow-agent.desktop"
# Success: drop the previous runtime and older AppImages.
RUNTIME_REPLACING=0
LINK_REPLACING=0
DEST_CREATING=0
rm -rf -- "$JOURNAL"
JOURNAL_CREATED=0
rm -rf -- "$LIB.previous"
for old in "$APPS"/ShadowCode-*-x86_64.AppImage; do
  [[ "$old" == "$DEST" || ! -f "$old" ]] || rm -- "$old"
done
command -v update-desktop-database >/dev/null && update-desktop-database "$DATA/applications" || true
printf 'Installed %s\nInstalled llama.cpp %s to %s\nConfig and task history are preserved. Restart the app to use the new release.\n' \
  "$DEST" "${RUNTIME_COMMIT:0:9}" "$LIB"
