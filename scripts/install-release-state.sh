#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Trusted installer library. Requires the installer's fixed roots and lock.
# Interface globals are consumed by the sourcing installer.
# shellcheck disable=SC2034,SC2153
# shellcheck source=native-release-auth-lib.sh
source "$ROOT/scripts/native-release-auth-lib.sh"
STATE="$(dirname "$LIB")/.shadowcode-release-state"
TRUST="$ROOT/release/trust"
ACCEPTED_ID=-
ACCEPTED_DIR=''
STATE_ADVANCING=0
STATE_DURABLE=0

state_field() {
  local file="$STATE/$1"
  [[ -f "$file" && ! -L "$file" && "$(stat -c '%u:%a:%h' -- "$file")" == "$(id -u):400:1" && "$(stat -c %s -- "$file")" -le 4096 ]] || fail 'Accepted-release state has an invalid root field; preserve it.'
  cat -- "$file"
}
validate_receipt() (
  local directory=$1 expected=$2 file count index SCRATCH
  private_dir "$directory" || fail 'Accepted-release receipt directory is not private.'
  [[ "$expected" =~ ^[a-f0-9]{64}$ ]] || fail 'Invalid accepted-release record identity.'
  count=$(find "$directory" -mindepth 1 -maxdepth 1 -printf x | wc -c)
  [[ "$count" == 4 ]] || fail 'Unexpected accepted-release receipt files.'
  for file in RELEASE-AUTH RELEASE-AUTH.sig RELEASE-MANIFEST.json SHA256SUMS; do
    [[ -f "$directory/$file" && ! -L "$directory/$file" && "$(stat -c '%u:%a:%h' -- "$directory/$file")" == "$(id -u):400:1" ]] || fail 'Accepted-release receipt file changed.'
  done
  [[ "$(file_hash "$directory/RELEASE-AUTH")" == "$expected" ]] || fail 'Accepted-release receipt identity changed.'
  SCRATCH=$(mktemp -d "$(dirname "$LIB")/.shadowcode-receipt-read.XXXXXX") || fail 'Could not stage receipt validation.'
  trap 'rm -rf -- "$SCRATCH"' EXIT
  [[ "$(openssl version)" == 'OpenSSL 3.'* ]] || fail 'OpenSSL 3 is required to authenticate recorded receipts.'
  load_trust_policy
  copy_file "$directory/RELEASE-AUTH" "$SCRATCH/RELEASE-AUTH" 4096
  copy_file "$directory/RELEASE-AUTH.sig" "$SCRATCH/RELEASE-AUTH.sig" 64
  # This path reads an already-recorded receipt for recovery/comparison only.
  # Fresh installations still pass the normal verifier and its current floors.
  verify_envelope "$SCRATCH/RELEASE-AUTH" "$SCRATCH/RELEASE-AUTH.sig" 1
  copy_file "$directory/RELEASE-MANIFEST.json" "$SCRATCH/RELEASE-MANIFEST.json" 1048576
  copy_file "$directory/SHA256SUMS" "$SCRATCH/SHA256SUMS" 4096
  [[ "$(hash_file "$SCRATCH/RELEASE-MANIFEST.json")" == "$EN_MANIFEST" && "$(hash_file "$SCRATCH/SHA256SUMS")" == "$EN_SUMS" ]] || fail 'Accepted-release metadata changed.'
  for index in 0 1 2; do printf '%s  %s\n' "${EN_HASHES[$index]}" "${EN_NAMES[$index]}"; done > "$SCRATCH/expected-sums"
  cmp -s -- "$SCRATCH/SHA256SUMS" "$SCRATCH/expected-sums" || fail 'Accepted-release checksum bindings changed.'
  printf '%s\t%s\t%s\t%s\n' "$EN_VERSION" "$EN_COMMIT" "${EN_NAMES[0]}" "${EN_HASHES[0]}"
)
load_accepted_receipt() {
  local link result
  ACCEPTED_ID=-; ACCEPTED_DIR=''
  if ! exists "$STATE"; then return; fi
  if ! private_dir "$STATE" || ! private_dir "$STATE/records"; then fail 'Accepted-release state is not private; preserve it.'; fi
  [[ "$(state_field schema)" == 1 && "$(state_field apps-root)" == "$(realpath -- "$APPS")" && "$(state_field library-root)" == "$(realpath -- "$(dirname "$LIB")")" ]] || fail 'Accepted-release installation roots changed; preserve state.'
  [[ -L "$STATE/accepted" && "$(stat -c %u -- "$STATE/accepted")" == "$(id -u)" ]] || fail 'Accepted-release pointer is missing or changed; preserve state.'
  link=$(readlink -- "$STATE/accepted")
  [[ "$link" =~ ^records/[a-f0-9]{64}$ ]] || fail 'Invalid accepted-release pointer; preserve state.'
  ACCEPTED_ID=${link#records/}; ACCEPTED_DIR="$STATE/$link"
  result=$(validate_receipt "$ACCEPTED_DIR" "$ACCEPTED_ID") || fail 'Accepted-release receipt could not be authenticated; preserve state.'
}
load_install_policy() {
  local file="$ROOT/release/install-policy" line
  [[ -f "$file" && ! -L "$file" && "$(stat -c %s -- "$file")" -le 256 ]] || fail 'Publisher install policy is not provisioned.'
  mapfile -t POLICY_LINES < "$file"
  [[ "${#POLICY_LINES[@]}" == 2 && "${POLICY_LINES[0]}" == ShadowCode-Install-Policy-v1 ]] || fail 'Invalid publisher install policy.'
  line=${POLICY_LINES[1]}
  [[ "$line" == first-authenticated-version=* ]] || fail 'Invalid authenticated-release boundary.'
  FIRST_AUTH_VERSION=${line#first-authenticated-version=}
  [[ "$FIRST_AUTH_VERSION" =~ ^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$ ]] || fail 'Invalid authenticated-release boundary.'
}
check_legacy_identity() {
  local previous version
  [[ "$ACCEPTED_ID" == - ]] || return 0
  if exists "$APPS/ShadowCode.AppImage"; then
    [[ -L "$APPS/ShadowCode.AppImage" ]] || fail 'Active AppImage is not a supported link.'
    previous=$(readlink -- "$APPS/ShadowCode.AppImage")
    [[ "$previous" =~ ^ShadowCode-((0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8}))-x86_64\.AppImage$ ]] || fail 'Active AppImage has an unsupported identity.'
    version=${BASH_REMATCH[1]}
    # This compares data only; no installed executable is queried.
    version_compare "$version" "$FIRST_AUTH_VERSION"
    [[ "$CMP" == -1 ]] || fail 'Authenticated installation state is missing; preserve the app and restore its receipt.'
    printf 'Prior installation is legacy/unverified; the incoming release still requires a publisher signature.\n' >&2
  fi
}
prepare_accepted_record() {
  local file
  CANDIDATE_ID=$(file_hash "$VERIFIED/RELEASE-AUTH") || fail 'Verified release envelope changed.'
  RECORD_STAGE="$STAGE/receipt"
  mkdir -m 700 -- "$RECORD_STAGE"
  for file in RELEASE-AUTH RELEASE-AUTH.sig RELEASE-MANIFEST.json SHA256SUMS; do
    install -m 400 -- "$VERIFIED/$file" "$RECORD_STAGE/$file"
  done
  validate_receipt "$RECORD_STAGE" "$CANDIDATE_ID" >/dev/null || fail 'Could not prepare accepted release receipt.'
  CANDIDATE_RECORD_HASH=$(runtime_hash "$RECORD_STAGE")
  STATE_PRIOR=$ACCEPTED_ID
  if [[ "$STATE_PRIOR" == - ]]; then
    mkdir -m 700 -- "$STAGE/release-state.new" "$STAGE/release-state.new/records"
    printf '1\n' > "$STAGE/release-state.new/schema"
    realpath -- "$APPS" > "$STAGE/release-state.new/apps-root"
    realpath -- "$(dirname "$LIB")" > "$STAGE/release-state.new/library-root"
    chmod 400 "$STAGE/release-state.new/schema" "$STAGE/release-state.new/apps-root" "$STAGE/release-state.new/library-root"
    mv -T -- "$RECORD_STAGE" "$STAGE/release-state.new/records/$CANDIDATE_ID"
    ln -s "records/$CANDIDATE_ID" "$STAGE/release-state.new/accepted"
    RECORD_STAGE="$STAGE/release-state.new/records/$CANDIDATE_ID"
  fi
  sync -f "$RECORD_STAGE"
  sync -f "$STAGE"
}
advance_accepted_record() {
  local destination
  STATE_ADVANCING=1
  if [[ "$STATE_PRIOR" == - ]]; then
    ! exists "$STATE" || fail 'Accepted-release state appeared concurrently.'
    mv -T -n -- "$STAGE/release-state.new" "$STATE"
    ! exists "$STAGE/release-state.new" || fail 'Accepted-release state appeared concurrently.'
    sync -f "$(dirname "$STATE")"
  else
    load_accepted_receipt
    [[ "$ACCEPTED_ID" == "$STATE_PRIOR" ]] || fail 'Accepted-release pointer changed concurrently.'
    destination="$STATE/records/$CANDIDATE_ID"
    if exists "$destination"; then
      validate_receipt "$destination" "$CANDIDATE_ID" >/dev/null || fail 'Existing accepted receipt changed.'
      [[ "$(runtime_hash "$destination")" == "$CANDIDATE_RECORD_HASH" ]] || fail 'Existing accepted receipt bytes changed.'
    else
      mv -T -n -- "$RECORD_STAGE" "$destination"
      ! exists "$RECORD_STAGE" || fail 'Accepted receipt appeared concurrently.'
    fi
    sync -f "$STATE/records"
    ln -s "records/$CANDIDATE_ID" "$STAGE/accepted.pending"
    mv -Tf -- "$STAGE/accepted.pending" "$STATE/accepted"
    sync -f "$STATE"
  fi
  STATE_DURABLE=1
}
validate_recovery_state() {
  local stage=$1 candidate=$2 candidate_hash=$3 existed=$4 old_hash=$5 live_hash=$6 previous_hash=$7
  local prior target record_hash directory found=0 result version _commit _artifact hash
  prior=$(read_field accepted-prior); target=$(read_field accepted-candidate); record_hash=$(read_field accepted-record-sha256)
  [[ ( "$prior" == - || "$prior" =~ ^[a-f0-9]{64}$ ) && "$target" =~ ^[a-f0-9]{64}$ && "$record_hash" =~ ^[a-f0-9]{64}$ ]] || recovery_refusal 'invalid accepted-release transaction'
  [[ "$(read_field state-root)" == "$STATE" ]] || recovery_refusal 'accepted-release root changed'
  load_accepted_receipt || recovery_refusal 'accepted-release state changed'
  [[ "$ACCEPTED_ID" == "$prior" || "$ACCEPTED_ID" == "$target" ]] || recovery_refusal 'accepted-release pointer changed'
  for directory in "$stage/receipt" "$stage/release-state.new/records/$target" "$STATE/records/$target"; do
    if exists "$directory"; then
      result=$(validate_receipt "$directory" "$target") || recovery_refusal 'candidate receipt changed'
      [[ "$(runtime_hash "$directory")" == "$record_hash" ]] || recovery_refusal 'candidate receipt bytes changed'
      IFS=$'\t' read -r version _commit _artifact hash <<< "$result"
      [[ "$candidate" == "ShadowCode-${version}-x86_64.AppImage" && "$candidate_hash" == "$hash" ]] || recovery_refusal 'candidate receipt does not bind AppImage'
      found=1
    fi
  done
  [[ "$found" == 1 ]] || recovery_refusal 'candidate receipt missing'
  if [[ "$prior" != - ]]; then validate_receipt "$STATE/records/$prior" "$prior" >/dev/null || recovery_refusal 'prior accepted receipt changed'; fi
  if [[ "$ACCEPTED_ID" != "$target" ]]; then
    [[ "$previous_hash" == - && "$live_hash" == "$old_hash" ]] || recovery_refusal 'replacement preceded durable accepted state'
    [[ "$existed" == 1 || ! -e "$APPS/$candidate" ]] || recovery_refusal 'candidate preceded durable accepted state'
  fi
  # Keep the candidate pointer when restoring older app bytes: never lower it.
}
