# Managed llama.cpp runtime

`scripts/build-llama.cpp.sh` compiles the [llama.cpp](https://github.com/ggml-org/llama.cpp)
commit pinned in `tools/llama.cpp.pin` and writes it into `bin/` here (not
committed). `scripts/build-native.mjs` copies `bin/` into both packages as
`usr/lib/shadowcode/`, keeping the relative SONAME symlinks, and
`scripts/install-appimage.sh` installs the copy from inside the AppImage into
`~/.local/lib/shadowcode/`.

Builds and `--notices-only` refreshes leave the tracked source pin and the
reference `packaging/llama.cpp/COMMIT` unchanged. The actual build timestamp
and backend are recorded in generated `bin/COMMIT`; updating pinned source
revisions requires an explicit reviewed change to the tracked inputs.

`bin/` contains:

- `llama-server`, `llama-cli` and their shared libraries (`$ORIGIN` rpath);
- backend modules loaded at runtime (`GGML_BACKEND_DL`): one CPU library per
  x86-64 microarchitecture level and `libggml-vulkan.so`, which is used only
  when a Vulkan loader and driver exist (otherwise the CPU is used);
- `architectures.txt`, the model architectures this commit can load;
- `COMMIT`: `url`, `commit`, `spirv_headers_commit`, `backend`, `built`;
- `NOTICES/`: the license texts of llama.cpp/ggml (MIT) and of the code it
  compiles in (cpp-httplib, nlohmann/json, stb_image, miniaudio, subprocess.h,
  and SPIRV-Headers for the Vulkan module). The same texts are pinned with
  SHA-256 digests in `licenses/native/sources.json`; packaging fails if they
  differ.

The runtime links the system `libssl.so.3`/`libcrypto.so.3`, `libgomp.so.1`
and the C/C++ runtime; the deb declares them as dependencies. It is not an
Ollama or LM Studio daemon, and the build never downloads model weights.

Build prerequisites: git, a C/C++ compiler, cmake (distribution package, or
`pip install cmake` into any venv), optionally ninja and patchelf; for the
Vulkan module, `libvulkan-dev` and `glslc` (shaderc, or the
`org.freedesktop.Sdk` flatpak runtime via `tools/glslc-flatpak.sh`).
