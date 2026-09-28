#!/usr/bin/env bash
# Build the pinned llama.cpp into ShadowCode's managed local runtime.
#
#   * No sudo, no curl|bash, no model downloads.
#   * Backends are built as loadable modules (GGML_BACKEND_DL): every x86-64
#     CPU variant plus a Vulkan module. At runtime llama-server picks the best
#     CPU variant for the host and loads the Vulkan module only when a Vulkan
#     loader and driver exist; otherwise it silently stays on the CPU.
#   * Vulkan needs a `glslc` shader compiler. `tools/glslc-flatpak.sh` finds one
#     on PATH or inside a user-installed org.freedesktop.Sdk flatpak runtime.
#     Set SHADOWCODE_LLAMA_VULKAN=0 to build a CPU-only runtime.
#   * SPIRV-Headers (header-only, Khronos) is fetched from its official
#     repository at the commit pinned in tools/llama.cpp.pin
#     (spirv_headers_commit=) and installed into tools/spirv-headers-prefix.
#   * cmake: SHADOWCODE_CMAKE, .venv/bin/cmake in this checkout, or cmake on
#     PATH (install your distribution's cmake, or pip install cmake into any
#     venv).
#   * The upstream license texts of everything compiled into the runtime are
#     copied into NOTICES/ next to the binaries: llama.cpp and ggml, the
#     vendored cpp-httplib, nlohmann/json, stb_image, miniaudio and
#     subprocess.h, and SPIRV-Headers when the Vulkan module is built.
#
# Outputs: packaging/llama.cpp/bin (bundled into the AppImage and the deb) and,
# unless --no-user-install is given, ~/.local/lib/shadowcode.
# The tracked source pin and reference manifest are inputs, never build outputs.
# Build timestamps and selected backends belong only in the generated runtime.
#
#   --no-user-install  only write packaging/llama.cpp/bin
#   --cpu-only         do not build the Vulkan module
#   --notices-only     do not compile: refresh NOTICES/ and COMMIT of an existing
#                      packaging/llama.cpp/bin that was built from the pinned
#                      commit (refuses when the built commit differs)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${SHADOWCODE_LLAMA_SRC:-$ROOT/tools/llama.cpp}"
BUILD_DIR="${SHADOWCODE_LLAMA_BUILD_DIR:-$SRC/build}"
PIN="$ROOT/tools/llama.cpp.pin"
OUT_REPO="$ROOT/packaging/llama.cpp"
OUT_USER="${SHADOWCODE_LLAMA_PREFIX:-$HOME/.local/lib/shadowcode}"
URL="${SHADOWCODE_LLAMA_URL:-https://github.com/ggml-org/llama.cpp.git}"
SPIRV_URL="${SHADOWCODE_SPIRV_HEADERS_URL:-https://github.com/KhronosGroup/SPIRV-Headers.git}"
SPIRV_SRC="$ROOT/tools/spirv-headers"
SPIRV_PREFIX="$ROOT/tools/spirv-headers-prefix"
WANT_VULKAN="${SHADOWCODE_LLAMA_VULKAN:-1}"
USER_INSTALL=1
NOTICES_ONLY=0
for arg in "$@"; do
  case "$arg" in
    --no-user-install) USER_INSTALL=0 ;;
    --cpu-only) WANT_VULKAN=0 ;;
    --notices-only) NOTICES_ONLY=1; USER_INSTALL=0 ;;
    *) echo "Unknown argument: $arg" >&2; exit 2 ;;
  esac
done
# key=value lookup in a COMMIT/pin file.
field() { [[ -f "$1" ]] && awk -F= -v key="$2" '$1 == key { print substr($0, length(key) + 2); exit }' "$1"; }
PINNED="${SHADOWCODE_LLAMA_COMMIT:-$(field "$PIN" commit || true)}"
SPIRV_PINNED="${SHADOWCODE_SPIRV_HEADERS_COMMIT:-$(field "$PIN" spirv_headers_commit || true)}"
SPIRV_COMMIT=""
SPIRV_NOTICE=""
CMAKE="${SHADOWCODE_CMAKE:-}"
if [[ -z "$CMAKE" ]]; then
  if [[ -x "$ROOT/.venv/bin/cmake" ]]; then
    CMAKE="$ROOT/.venv/bin/cmake"
  elif command -v cmake >/dev/null 2>&1; then
    CMAKE="$(command -v cmake)"
  elif [[ "$NOTICES_ONLY" == "0" ]]; then
    echo "cmake is required: install your distribution's cmake, or pip install cmake into any venv and set SHADOWCODE_CMAKE." >&2
    exit 1
  fi
fi
GENERATOR=()
if command -v ninja >/dev/null 2>&1; then
  GENERATOR=(-G Ninja)
fi
JOBS="${SHADOWCODE_BUILD_JOBS:-$(( $(nproc) > 2 ? $(nproc) - 2 : 1 ))}"

mkdir -p "$(dirname "$SRC")" "$OUT_REPO/bin"
if [[ ! -d "$SRC/.git" ]]; then
  git clone --filter=blob:none "$URL" "$SRC"
fi
git -C "$SRC" remote set-url origin "$URL"
if [[ -n "$PINNED" ]]; then
  if ! git -C "$SRC" cat-file -e "$PINNED^{commit}" 2>/dev/null; then
    git -C "$SRC" fetch --depth 1 origin "$PINNED"
  fi
  git -C "$SRC" checkout --detach "$PINNED"
else
  git -C "$SRC" fetch --depth 1 origin master
  git -C "$SRC" checkout --detach FETCH_HEAD
fi
COMMIT="$(git -C "$SRC" rev-parse HEAD)"

# SPIRV-Headers at the pinned commit (the default branch only while no pin
# exists yet; the actual commit is recorded in generated runtime metadata).
fetch_spirv_headers() {
  if [[ ! -d "$SPIRV_SRC/.git" ]]; then
    git init -q "$SPIRV_SRC"
    git -C "$SPIRV_SRC" remote add origin "$SPIRV_URL"
  fi
  git -C "$SPIRV_SRC" remote set-url origin "$SPIRV_URL"
  if [[ -n "$SPIRV_PINNED" ]]; then
    if ! git -C "$SPIRV_SRC" cat-file -e "$SPIRV_PINNED^{commit}" 2>/dev/null; then
      git -C "$SPIRV_SRC" fetch --depth 1 origin "$SPIRV_PINNED"
    fi
    git -C "$SPIRV_SRC" checkout -q --detach "$SPIRV_PINNED"
  else
    git -C "$SPIRV_SRC" fetch --depth 1 origin HEAD
    git -C "$SPIRV_SRC" checkout -q --detach FETCH_HEAD
  fi
  SPIRV_COMMIT="$(git -C "$SPIRV_SRC" rev-parse HEAD)"
  if [[ -n "$SPIRV_PINNED" && "$SPIRV_COMMIT" != "$SPIRV_PINNED" ]]; then
    echo "SPIRV-Headers checkout $SPIRV_COMMIT does not match the pin $SPIRV_PINNED" >&2
    exit 1
  fi
  SPIRV_NOTICE="$SPIRV_SRC/LICENSE"
}
install_spirv_headers() {
  if [[ "$(cat "$SPIRV_PREFIX/COMMIT" 2>/dev/null)" == "$SPIRV_COMMIT" \
        && -f "$SPIRV_PREFIX/include/spirv/unified1/spirv.hpp" ]]; then
    return
  fi
  rm -rf "$SPIRV_SRC/build" "$SPIRV_PREFIX"
  "$CMAKE" -S "$SPIRV_SRC" -B "$SPIRV_SRC/build" \
    -DCMAKE_INSTALL_PREFIX="$SPIRV_PREFIX" \
    -DSPIRV_HEADERS_ENABLE_TESTS=OFF -DSPIRV_HEADERS_ENABLE_INSTALL=ON >/dev/null
  "$CMAKE" --build "$SPIRV_SRC/build" --target install >/dev/null
  printf '%s\n' "$SPIRV_COMMIT" > "$SPIRV_PREFIX/COMMIT"
}

# Upstream license texts for the code compiled into the runtime. The header-only
# libraries carry their license at the end (stb_image, miniaudio) or the start
# (subprocess.h) of the header; that section is copied verbatim.
write_notices() {
  local out="$1/NOTICES"
  rm -rf "$out"
  mkdir -p "$out"
  cp "$SRC/LICENSE" "$out/llama.cpp-LICENSE"
  cp "$SRC/licenses/LICENSE-jsonhpp" "$out/nlohmann-json-LICENSE"
  cp "$SRC/vendor/cpp-httplib/LICENSE" "$out/cpp-httplib-LICENSE"
  sed -n '/This software is available under 2 licenses/,$p' "$SRC/vendor/stb/stb_image.h" > "$out/stb_image-LICENSE"
  sed -n '/This software is available as a choice of the following licenses/,$p' "$SRC/vendor/miniaudio/miniaudio.h" > "$out/miniaudio-LICENSE"
  sed -n '1,/For more information, please refer to/p' "$SRC/vendor/sheredom/subprocess.h" > "$out/subprocess.h-LICENSE"
  if [[ "$BACKEND" == *vulkan* ]]; then
    cp "$SPIRV_NOTICE" "$out/SPIRV-Headers-LICENSE"
  fi
  local file
  for file in "$out"/*; do
    [[ -s "$file" ]] || { echo "Empty runtime notice: $file" >&2; exit 1; }
  done
}

write_commit() {
  {
    printf 'url=%s\n' "$URL"
    printf 'commit=%s\n' "$COMMIT"
    if [[ -n "$SPIRV_COMMIT" ]]; then
      printf 'spirv_headers_commit=%s\n' "$SPIRV_COMMIT"
    fi
    printf 'backend=%s\n' "$BACKEND"
    printf 'built=%s\n' "$BUILT"
  } > "$1"
}

if [[ "$NOTICES_ONLY" == "1" ]]; then
  EXISTING="$OUT_REPO/bin/COMMIT"
  [[ -x "$OUT_REPO/bin/llama-server" && -f "$EXISTING" ]] || { echo "No built runtime in $OUT_REPO/bin" >&2; exit 1; }
  if [[ "$(field "$EXISTING" commit)" != "$COMMIT" ]]; then
    echo "The runtime in $OUT_REPO/bin was built from $(field "$EXISTING" commit), not the pinned $COMMIT; rebuild it." >&2
    exit 1
  fi
  BACKEND="$(field "$EXISTING" backend)"
  BUILT="$(field "$EXISTING" built)"
  if [[ "$BACKEND" == *vulkan* ]]; then
    fetch_spirv_headers
  fi
  write_notices "$OUT_REPO/bin"
  write_commit "$OUT_REPO/bin/COMMIT"
  printf 'Refreshed notices of managed llama.cpp %s (%s) in %s\n' "$COMMIT" "$BACKEND" "$OUT_REPO/bin"
  exit 0
fi

VULKAN_FLAGS=(-DGGML_VULKAN=OFF)
BACKEND="cpu"
if [[ "$WANT_VULKAN" == "1" ]]; then
  GLSLC="$ROOT/tools/glslc-flatpak.sh"
  if ! "$GLSLC" --version >/dev/null 2>&1; then
    echo "No glslc shader compiler found; building a CPU-only runtime. Install shaderc or the org.freedesktop.Sdk flatpak runtime for Vulkan." >&2
    WANT_VULKAN=0
  fi
fi
if [[ "$WANT_VULKAN" == "1" ]]; then
  if [[ ! -f /usr/include/vulkan/vulkan.h && ! -f /usr/local/include/vulkan/vulkan.h ]]; then
    echo "Vulkan headers (libvulkan-dev) are missing; building a CPU-only runtime." >&2
    WANT_VULKAN=0
  fi
fi
if [[ "$WANT_VULKAN" == "1" ]]; then
  fetch_spirv_headers
  install_spirv_headers
  SPIRV_INCLUDE="$SPIRV_PREFIX/include"
  # The pinned ggml-vulkan CMake only runs find_package(SPIRV-Headers); the
  # header must also be on the compiler include path.
  VULKAN_FLAGS=(
    -DGGML_VULKAN=ON
    -DVulkan_GLSLC_EXECUTABLE="$GLSLC"
    -DCMAKE_PREFIX_PATH="$SPIRV_PREFIX"
    -DCMAKE_C_FLAGS="-isystem $SPIRV_INCLUDE"
    -DCMAKE_CXX_FLAGS="-isystem $SPIRV_INCLUDE"
  )
  BACKEND="vulkan+cpu"
fi

"$CMAKE" -S "$SRC" -B "$BUILD_DIR" "${GENERATOR[@]}" \
  -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_SHARED_LIBS=ON \
  -DGGML_BACKEND_DL=ON \
  -DGGML_CPU_ALL_VARIANTS=ON \
  -DGGML_NATIVE=OFF \
  -DGGML_CUDA=OFF \
  "${VULKAN_FLAGS[@]}" \
  -DLLAMA_BUILD_TESTS=OFF \
  -DLLAMA_BUILD_EXAMPLES=OFF \
  -DLLAMA_BUILD_TOOLS=ON \
  -DLLAMA_BUILD_SERVER=ON \
  -DLLAMA_CURL=OFF \
  -DCMAKE_INSTALL_RPATH='$ORIGIN' \
  -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON
"$CMAKE" --build "$BUILD_DIR" --parallel "$JOBS"
BIN_DIR="$BUILD_DIR/bin"
[[ -x "$BIN_DIR/llama-server" && -x "$BIN_DIR/llama-cli" ]] || { echo "llama.cpp build did not produce llama-cli/llama-server" >&2; exit 1; }
BUILT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

install_runtime() {
  local dest="$1"
  rm -rf "$dest.new"
  mkdir -p "$dest.new"
  install -m 755 "$BIN_DIR/llama-cli" "$dest.new/llama-cli"
  install -m 755 "$BIN_DIR/llama-server" "$dest.new/llama-server"
  # Core libraries, the two tool implementations, and every loadable backend
  # module (libggml-cpu-*.so, libggml-vulkan.so). Other tool libraries from
  # the build tree (bench, quantize, perplexity, ...) are not shipped.
  # cp -a keeps the SONAME symlinks relative.
  find "$BIN_DIR" -maxdepth 1 \( -name 'libllama.so*' -o -name 'libllama-common.so*' \
    -o -name 'libllama-server-impl.so' -o -name 'libllama-cli-impl.so' \
    -o -name 'libggml*.so*' -o -name 'libmtmd*.so*' \) -exec cp -a {} "$dest.new/" \;
  # Model architectures the pinned runtime can load, from its own source, so
  # the catalog can say "unsupported architecture" instead of guessing.
  sed -n '/LLM_ARCH_NAMES/,/^};/p' "$SRC/src/llama-arch.cpp" \
    | grep -oE '\{ *LLM_ARCH_[A-Z0-9_]+ *, *"[^"]+"' | sed -E 's/.*"([^"]+)"/\1/' \
    | grep -v '^(unknown)$' | sort -u > "$dest.new/architectures.txt"
  if command -v patchelf >/dev/null 2>&1; then
    for bin in llama-cli llama-server; do
      patchelf --set-rpath '$ORIGIN' "$dest.new/$bin"
    done
    for lib in "$dest.new"/lib*.so*; do
      [[ -f "$lib" && ! -L "$lib" ]] || continue
      patchelf --set-rpath '$ORIGIN' "$lib"
    done
  fi
  write_notices "$dest.new"
  write_commit "$dest.new/COMMIT"
  # Replace atomically so a running app never sees a half-copied runtime.
  if [[ -d "$dest" ]]; then
    rm -rf "$dest.old"
    mv "$dest" "$dest.old"
  fi
  mv "$dest.new" "$dest"
  rm -rf "$dest.old"
}
install_runtime "$OUT_REPO/bin"
if [[ "$USER_INSTALL" == "1" ]]; then
  install_runtime "$OUT_USER"
fi
"$OUT_REPO/bin/llama-server" --version
printf 'Built managed llama.cpp %s (%s) -> %s\n' "$COMMIT" "$BACKEND" "$OUT_REPO/bin"
if [[ "$USER_INSTALL" == "1" ]]; then
  printf 'Installed managed llama.cpp -> %s\n' "$OUT_USER"
fi
