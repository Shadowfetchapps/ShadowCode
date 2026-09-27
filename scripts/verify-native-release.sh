#!/usr/bin/env bash
# Offline verification only. No execution, installation, download, or trust update.
set -euo pipefail
export LC_ALL=C
umask 077
fail() { printf 'Release authentication refused: %s\n' "$*" >&2; exit 1; }
usage() { printf '%s\n' 'Usage: verify-native-release.sh --bundle-dir DIR --trust-dir TRUST --artifact BASENAME --stage-dir NEW-DIR [--previous-dir DIR] [--expect-version VERSION] [--expect-commit SHA]' >&2; exit 2; }
BUNDLE='' TRUST='' ARTIFACT='' OUTPUT='' PREVIOUS='' EXPECT_VERSION='' EXPECT_COMMIT=''
declare -A SEEN=()
while (( $# )); do
  [[ $# -ge 2 && -n "$2" ]] || usage
  [[ -z "${SEEN[$1]:-}" ]] || usage
  SEEN[$1]=1
  case "$1" in
    --bundle-dir) BUNDLE=$2 ;; --trust-dir) TRUST=$2 ;; --artifact) ARTIFACT=$2 ;;
    --stage-dir) OUTPUT=$2 ;; --previous-dir) PREVIOUS=$2 ;;
    --expect-version) EXPECT_VERSION=$2 ;; --expect-commit) EXPECT_COMMIT=$2 ;;
    *) usage ;;
  esac
  shift 2
done
[[ -n "$BUNDLE" && -n "$TRUST" && -n "$ARTIFACT" && -n "$OUTPUT" ]] || usage
[[ "$ARTIFACT" =~ ^[A-Za-z0-9_.-]+$ ]] || fail 'artifact must be a basename'
[[ -d "$BUNDLE" && -d "$TRUST" ]] || fail 'bundle and independently trusted key directory are required'
[[ ! -e "$OUTPUT" && ! -L "$OUTPUT" ]] || fail 'staging destination already exists'
command -v openssl >/dev/null || fail 'OpenSSL 3 is required'
[[ "$(openssl version)" == 'OpenSSL 3.'* ]] || fail 'OpenSSL 3 is required'
PARENT=$(realpath -- "$(dirname -- "$OUTPUT")") || fail 'staging parent missing'
BASE=$(basename -- "$OUTPUT")
[[ "$BASE" != . && "$BASE" != .. ]] || fail 'invalid staging destination'
OUTPUT="$PARENT/$BASE"
SCRATCH=$(mktemp -d "$PARENT/.shadowcode-auth.XXXXXX")
finish() { rm -rf -- "$SCRATCH"; }
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

HEX='^[a-f0-9]{64}$'
VERSION_PATTERN='^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$'
EPOCH_PATTERN='^[1-9][0-9]{0,8}$'
MAX_ARTIFACT=34359738368
copy_file() {
  local source=$1 destination=$2 limit=$3 size copied
  [[ -f "$source" && ! -L "$source" ]] || fail 'input must be a regular non-symlink file'
  size=$(stat -c %s -- "$source")
  [[ "$size" =~ ^[0-9]+$ && "$size" -gt 0 && "$size" -le "$limit" ]] || fail 'input size limit'
  rm -f -- "$destination"
  # O_NOFOLLOW closes the pathname/symlink race; O_NONBLOCK prevents a swapped
  # FIFO from hanging the open/read. Stop after the observed size plus one byte.
  dd if="$source" of="$destination" iflag=nofollow,nonblock,count_bytes count="$((size + 1))" bs=65536 status=none || fail 'could not snapshot input'
  copied=$(stat -c %s -- "$destination")
  [[ "$copied" == "$size" && "$copied" -le "$limit" ]] || fail 'input size changed or exceeded limit'
  chmod 400 "$destination"
}
hash_file() { sha256sum < "$1" | cut -d ' ' -f 1; }
read_lines() {
  local file=$1 limit=$2 invalid
  [[ "$(stat -c %s -- "$file")" -le "$limit" ]] || fail 'metadata size limit'
  invalid=$(tr -d '\11\12\40-\176' < "$file" | wc -c)
  [[ "$invalid" == 0 ]] || fail 'metadata contains non-ASCII or control bytes'
  [[ "$(tail -c 1 -- "$file" | od -An -tx1 | tr -d ' \n')" == 0a ]] || fail 'metadata needs final newline'
  mapfile -t ROWS < "$file"
}
take() {
  local prefix="$2="
  [[ "${ROWS[$1]:-}" == "$prefix"* ]] || fail "expected $2"
  VALUE=${ROWS[$1]#"$prefix"}
  [[ -n "$VALUE" && "$VALUE" != *$'\t'* && "$VALUE" != *=* ]] || fail "invalid $2"
}
identity() {
  take 1 repository; [[ "$VALUE" == Shadowfetchapps/ShadowCode ]] || fail 'wrong repository'
  take 2 repository-id; [[ "$VALUE" == 1377099349 ]] || fail 'wrong repository ID'
  take 3 owner-id; [[ "$VALUE" == 209457103 ]] || fail 'wrong owner ID'
}
version_compare() {
  local a=$1 b=$2 i
  local -a left right
  [[ "$a" =~ $VERSION_PATTERN && "$b" =~ $VERSION_PATTERN ]] || fail 'invalid stable version'
  IFS=. read -r -a left <<< "$a"; IFS=. read -r -a right <<< "$b"
  CMP=0
  for i in 0 1 2; do
    if (( left[i] < right[i] )); then CMP=-1; return; fi
    if (( left[i] > right[i] )); then CMP=1; return; fi
  done
}
parse_envelope() {
  read_lines "$1" 4096
  [[ "${#ROWS[@]}" == 16 && "${ROWS[0]}" == ShadowCode-Release-Auth-v1 ]] || fail 'envelope schema/field count'
  identity
  take 4 version; EN_VERSION=$VALUE; [[ "$EN_VERSION" =~ $VERSION_PATTERN ]] || fail 'invalid stable version'
  take 5 tag; [[ "$VALUE" == "v$EN_VERSION" ]] || fail 'version/tag mismatch'
  take 6 commit; EN_COMMIT=$VALUE; [[ "$EN_COMMIT" =~ ^[a-f0-9]{40}$ ]] || fail 'invalid commit'
  take 7 target; [[ "$VALUE" == x86_64-unknown-linux-gnu ]] || fail 'wrong target'
  take 8 channel; [[ "$VALUE" == stable ]] || fail 'wrong channel'
  take 9 key-id; EN_KEY=$VALUE; [[ "$EN_KEY" =~ $HEX ]] || fail 'invalid key ID'
  take 10 key-epoch; EN_EPOCH=$VALUE; [[ "$EN_EPOCH" =~ $EPOCH_PATTERN ]] || fail 'invalid key epoch'
  take 11 manifest-sha256; EN_MANIFEST=$VALUE; [[ "$EN_MANIFEST" =~ $HEX ]] || fail 'invalid manifest digest'
  take 12 checksums-sha256; EN_SUMS=$VALUE; [[ "$EN_SUMS" =~ $HEX ]] || fail 'invalid checksums digest'
  EN_NAMES=("ShadowCode_${EN_VERSION}_amd64.AppImage" "ShadowCode_${EN_VERSION}_amd64.deb" "ShadowCode_${EN_VERSION}_appimage-runtime-sources.tar.gz")
  EN_HASHES=(); EN_SIZES=()
  local -a roles=(appimage deb runtime-sources)
  local i value role name size hash extra
  for i in 0 1 2; do
    [[ "${ROWS[$((13 + i))]}" == asset=* ]] || fail 'missing asset'
    value=${ROWS[$((13 + i))]#asset=}
    IFS=$'\t' read -r role name size hash extra <<< "$value"
    [[ -z "$extra" && "$role" == "${roles[$i]}" && "$name" == "${EN_NAMES[$i]}" && "$size" =~ ^[1-9][0-9]{0,10}$ && "$size" -le "$MAX_ARTIFACT" && "$hash" =~ $HEX ]] || fail 'invalid/duplicate/path-bearing asset'
    [[ "$value" == "$role"$'\t'"$name"$'\t'"$size"$'\t'"$hash" ]] || fail 'noncanonical asset fields'
    EN_HASHES+=("$hash"); EN_SIZES+=("$size")
  done
}
copy_file "$TRUST/policy" "$SCRATCH/trust-policy" 4096
read_lines "$SCRATCH/trust-policy" 4096
[[ "${#ROWS[@]}" -ge 10 && "${#ROWS[@]}" -le 17 && "${ROWS[0]}" == ShadowCode-Release-Trust-v1 ]] || fail 'trust policy schema/field count'
identity
take 4 target; [[ "$VALUE" == x86_64-unknown-linux-gnu ]] || fail 'wrong trusted target'
take 5 channel; [[ "$VALUE" == stable ]] || fail 'wrong trusted channel'
take 6 minimum-epoch; MIN_EPOCH=$VALUE; [[ "$MIN_EPOCH" =~ $EPOCH_PATTERN ]] || fail 'invalid epoch floor'
take 7 minimum-version; MIN_VERSION=$VALUE; [[ "$MIN_VERSION" =~ $VERSION_PATTERN ]] || fail 'invalid version floor'
[[ "${ROWS[8]}" == keys=ed25519-spki-sha256 ]] || fail 'unsupported trust key encoding'
KEY_IDS=(); KEY_EPOCHS=(); KEY_MINS=(); KEY_MAXS=()
declare -A KEY_SEEN=()
for row in "${ROWS[@]:9}"; do
  [[ "$row" == key=* ]] || fail 'unknown trust field'
  value=${row#key=}
  IFS=$'\t' read -r epoch key minimum maximum extra <<< "$value"
  [[ -z "$extra" && "$epoch" =~ $EPOCH_PATTERN && "$key" =~ $HEX && "$minimum" =~ $VERSION_PATTERN && "$maximum" =~ $VERSION_PATTERN ]] || fail 'invalid trust key'
  [[ "$value" == "$epoch"$'\t'"$key"$'\t'"$minimum"$'\t'"$maximum" && -z "${KEY_SEEN[$key]:-}" ]] || fail 'duplicate/noncanonical trust key'
  version_compare "$minimum" "$maximum"; (( CMP <= 0 )) || fail 'invalid key version interval'
  KEY_SEEN[$key]=1; KEY_IDS+=("$key"); KEY_EPOCHS+=("$epoch"); KEY_MINS+=("$minimum"); KEY_MAXS+=("$maximum")
done
verify_envelope() {
  local file=$1 signature=$2 historical=$3 i found=-1 der_hex
  parse_envelope "$file"
  [[ "$(stat -c %s -- "$signature")" == 64 ]] || fail 'signature must be exactly 64 bytes'
  for i in "${!KEY_IDS[@]}"; do [[ "${KEY_IDS[$i]}" != "$EN_KEY" ]] || found=$i; done
  (( found >= 0 )) || fail 'unknown signing key'
  [[ "$EN_EPOCH" == "${KEY_EPOCHS[$found]}" ]] || fail 'key epoch mismatch'
  version_compare "$EN_VERSION" "${KEY_MINS[$found]}"; (( CMP >= 0 )) || fail 'below key version interval'
  version_compare "$EN_VERSION" "${KEY_MAXS[$found]}"; (( CMP <= 0 )) || fail 'above key version interval'
  if [[ "$historical" == 0 ]]; then
    (( EN_EPOCH >= MIN_EPOCH )) || fail 'retired signing epoch'
    version_compare "$EN_VERSION" "$MIN_VERSION"; (( CMP >= 0 )) || fail 'below trusted version floor'
  fi
  copy_file "$TRUST/$EN_KEY.pem" "$SCRATCH/public-key.pem" 1024
  rm -f -- "$SCRATCH/public-key.der"
  openssl pkey -pubin -in "$SCRATCH/public-key.pem" -outform DER -out "$SCRATCH/public-key.der" >/dev/null 2>&1 || fail 'invalid trusted public key'
  der_hex=$(od -An -v -tx1 "$SCRATCH/public-key.der" | tr -d ' \n')
  [[ "$der_hex" =~ ^302a300506032b6570032100[a-f0-9]{64}$ ]] || fail 'only Ed25519 SPKI keys are allowed'
  openssl pkey -pubin -in "$SCRATCH/public-key.pem" -pubout -out "$SCRATCH/public-key.canonical.pem" >/dev/null 2>&1 || fail 'invalid trusted public key'
  cmp -s -- "$SCRATCH/public-key.pem" "$SCRATCH/public-key.canonical.pem" || fail 'trusted key must be canonical public SPKI PEM'
  [[ "$(hash_file "$SCRATCH/public-key.der")" == "$EN_KEY" ]] || fail 'public key fingerprint mismatch'
  openssl pkeyutl -verify -rawin -pubin -inkey "$SCRATCH/public-key.pem" -in "$file" -sigfile "$signature" >/dev/null 2>&1 || fail 'invalid release signature'
}
copy_file "$BUNDLE/RELEASE-AUTH" "$SCRATCH/RELEASE-AUTH" 4096
copy_file "$BUNDLE/RELEASE-AUTH.sig" "$SCRATCH/RELEASE-AUTH.sig" 64
verify_envelope "$SCRATCH/RELEASE-AUTH" "$SCRATCH/RELEASE-AUTH.sig" 0
CURRENT_VERSION=$EN_VERSION; CURRENT_EPOCH=$EN_EPOCH; CURRENT_HASH=$(hash_file "$SCRATCH/RELEASE-AUTH")
[[ -z "$EXPECT_VERSION" || "$EXPECT_VERSION" == "$EN_VERSION" ]] || fail 'unexpected version'
[[ -z "$EXPECT_COMMIT" || "$EXPECT_COMMIT" == "$EN_COMMIT" ]] || fail 'unexpected commit'
if [[ -n "$PREVIOUS" ]]; then
  copy_file "$PREVIOUS/RELEASE-AUTH" "$SCRATCH/previous-auth" 4096
  copy_file "$PREVIOUS/RELEASE-AUTH.sig" "$SCRATCH/previous-auth.sig" 64
  verify_envelope "$SCRATCH/previous-auth" "$SCRATCH/previous-auth.sig" 1
  (( CURRENT_EPOCH >= EN_EPOCH )) || fail 'signing epoch rollback'
  version_compare "$CURRENT_VERSION" "$EN_VERSION"; (( CMP >= 0 )) || fail 'release downgrade'
  if (( CMP == 0 )); then [[ "$CURRENT_HASH" == "$(hash_file "$SCRATCH/previous-auth")" ]] || fail 'changed release under accepted version'; fi
  parse_envelope "$SCRATCH/RELEASE-AUTH"
fi
copy_file "$BUNDLE/RELEASE-MANIFEST.json" "$SCRATCH/RELEASE-MANIFEST.json" 1048576
copy_file "$BUNDLE/SHA256SUMS" "$SCRATCH/SHA256SUMS" 4096
[[ "$(hash_file "$SCRATCH/RELEASE-MANIFEST.json")" == "$EN_MANIFEST" ]] || fail 'manifest digest mismatch'
[[ "$(hash_file "$SCRATCH/SHA256SUMS")" == "$EN_SUMS" ]] || fail 'checksums digest mismatch'
SELECTED=-1
for i in 0 1 2; do
  printf '%s  %s\n' "${EN_HASHES[$i]}" "${EN_NAMES[$i]}" >> "$SCRATCH/expected-sums"
  [[ "$ARTIFACT" != "${EN_NAMES[$i]}" ]] || SELECTED=$i
done
cmp -s -- "$SCRATCH/SHA256SUMS" "$SCRATCH/expected-sums" || fail 'checksums asset set mismatch'
(( SELECTED >= 0 )) || fail 'artifact must be one exact authenticated basename'
copy_file "$BUNDLE/$ARTIFACT" "$SCRATCH/$ARTIFACT" "$MAX_ARTIFACT"
[[ "$(stat -c %s -- "$SCRATCH/$ARTIFACT")" == "${EN_SIZES[$SELECTED]}" && "$(hash_file "$SCRATCH/$ARTIFACT")" == "${EN_HASHES[$SELECTED]}" ]] || fail 'artifact digest/size mismatch'
rm -f -- "$SCRATCH/trust-policy" "$SCRATCH/public-key.pem" "$SCRATCH/public-key.der" "$SCRATCH/public-key.canonical.pem" "$SCRATCH/expected-sums" "$SCRATCH/previous-auth" "$SCRATCH/previous-auth.sig"
# Same-filesystem rename; no existing output may be replaced. The successful
# output contains only verified private snapshots, never the live inputs.
mv -T -n -- "$SCRATCH" "$OUTPUT" || fail 'could not retain verified stage'
[[ ! -e "$SCRATCH" ]] || fail 'staging destination appeared concurrently'
trap - EXIT INT TERM
printf 'Publisher signature verified: version=%s artifact=%s sha256=%s\n' "$EN_VERSION" "$ARTIFACT" "${EN_HASHES[$SELECTED]}"
