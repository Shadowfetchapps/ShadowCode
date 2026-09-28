#!/bin/sh
# Exercise the packaged MCP stdio protocol without Node or network access.
set -eu

binary=$1
profile=$2
workspace=$3
temp=$(mktemp -d)
input=$temp/input
response=$temp/response
mkfifo "$input" "$response"
cleanup() {
  if [ -n "${pid:-}" ]; then
    # timeout owns the server process group and also enforces a hard deadline.
    kill -TERM "$pid" 2>/dev/null || :
    wait "$pid" 2>/dev/null || :
  fi
  rm -rf "$temp"
}
trap cleanup EXIT HUP INT TERM
mkdir -p "$profile" "$workspace"
git -C "$workspace" init -q
printf 'clean-host MCP fixture\n' > "$workspace/README.md"

# Keep both FIFOs open so the server does not see EOF between requests.
exec 3<>"$input"
timeout --kill-after=2s 20s "$binary" --profile "$profile" --workspace "$workspace" mcp serve 3>&- <"$input" >"$response" &
pid=$!
exec 4<"$response"
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"clean-host-smoke","version":"1"}}}' >&3
IFS= read -r initialized <&4
printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}' >&3
printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' >&3
IFS= read -r listed <&4
exec 3>&-
exec 4<&-
wait "$pid"
pid=
# The host validates JSON-RPC structure and the actual catalog. Grepping these
# responses here can mistake error messages or malformed JSON for success.
printf '__SHADOW_CLEAN_MCP_INITIALIZE__%s\n' "$initialized"
printf '__SHADOW_CLEAN_MCP_TOOLS__%s\n' "$listed"
