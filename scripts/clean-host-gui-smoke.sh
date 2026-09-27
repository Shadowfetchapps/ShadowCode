#!/bin/sh
# Runs inside the offline qualification container under an unprivileged user.
set -eu

binary=$1
mkdir -p /tmp/project /tmp/profile
git -C /tmp/project init -q
printf 'clean-host first-launch fixture\n' > /tmp/project/README.md

"$binary" --profile /tmp/profile --workspace /tmp/project ui >/tmp/shadowcode-gui.log 2>&1 &
app_pid=$!
cleanup() {
  kill "$app_pid" 2>/dev/null || :
  wait "$app_pid" 2>/dev/null || :
}
trap cleanup EXIT HUP INT TERM

attempt=0
while [ "$attempt" -lt 55 ]; do
  window_ids=$(xdotool search --onlyvisible --name '^ShadowCode$' 2>/dev/null || :)
  if [ -n "$window_ids" ]; then
    window_id=$(printf '%s\n' "$window_ids" | head -n 1)
    [ "$(xdotool getwindowname "$window_id")" = 'ShadowCode' ]
    xwininfo -id "$window_id" | grep -Eq 'Width: [1-9][0-9]*'
    kill -0 "$app_pid"
    printf '__SHADOW_CLEAN_WINDOW__\n'
    exit 0
  fi
  if ! kill -0 "$app_pid" 2>/dev/null; then
    cat /tmp/shadowcode-gui.log >&2
    echo 'ShadowCode exited before creating a window' >&2
    exit 1
  fi
  attempt=$((attempt + 1))
  sleep 1
done

cat /tmp/shadowcode-gui.log >&2
echo 'ShadowCode did not create a window within 55 seconds' >&2
exit 1
