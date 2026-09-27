# Runtime-only Debian qualification image. The production app must not rely on
# the repository, Node, Cargo, compiler headers, or the CI runner's dev packages.
FROM --platform=linux/amd64 debian@sha256:38a76d01668772e381ad2826d876627c89e7133e2f8a0f5d567306798b0f2a16
RUN apt-get update -qq \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      git libgomp1 libssl3t64 libasound2t64 libwebkit2gtk-4.1-0 libgtk-3-0 \
 && rm -rf /var/lib/apt/lists/* \
 && ! command -v node \
 && ! command -v cargo \
 && ! command -v rustc
