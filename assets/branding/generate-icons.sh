#!/usr/bin/env bash
# Rebuild packaging derivatives from the approved, preserved source images.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
export MAGICK_THREAD_LIMIT=2
sha256sum -c assets/branding/sources.sha256
EMBLEM=assets/branding/shadowcode-emblem.png

icon() {
  local size="$1" output="$2"
  convert "$EMBLEM" -colorspace sRGB -filter Lanczos -resize "${size}x${size}" \
    -strip -define png:exclude-chunks=date,time "PNG32:$output"
}

for size in 16 22 24 32 48 64 96 128 256 512; do
  icon "$size" "assets/icons/shadow-agent-${size}.png"
  directory="assets/icons/hicolor/${size}x${size}/apps"
  mkdir -p "$directory"
  cp "assets/icons/shadow-agent-${size}.png" "$directory/shadow-agent.png"
done
cp assets/icons/shadow-agent-512.png assets/icons/shadow-agent.png

# Keep legacy SVG paths self-contained for desktop installers and old links.
# 256 px is sufficient for this compatibility wrapper; native bundles also
# retain dedicated 512 px PNGs. Do not duplicate the full-resolution master.
svg=assets/icons/shadow-agent.svg
{
  printf '%s\n' '<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 256 256" width="256" height="256" role="img" aria-label="ShadowCode">' '<title>ShadowCode</title>'
  printf '%s' '<image width="256" height="256" xlink:href="data:image/png;base64,'
  base64 -w 0 assets/icons/shadow-agent-256.png
  printf '%s\n' '" /></svg>'
} > "$svg"
mkdir -p assets/icons/hicolor/scalable/apps
for path in assets/icons/hicolor/scalable/apps/shadow-agent.svg assets/shadow-agent.svg icon.svg ui/public/icon.svg; do
  cp "$svg" "$path"
done

icon 192 ui/public/icon-192.png
cp assets/icons/shadow-agent-512.png ui/public/icon-512.png
cp assets/icons/shadow-agent-32.png ui/public/icon-32.png
convert assets/icons/shadow-agent-16.png assets/icons/shadow-agent-32.png \
  assets/icons/shadow-agent-48.png ui/public/favicon.ico
# Apple icons are opaque. The maskable icon keeps the emblem inside the
# central safe circle, with a charcoal fill for platform cropping.
convert "$EMBLEM" -colorspace sRGB -filter Lanczos -resize 180x180 \
  -background '#0d1117' -alpha remove -alpha off -strip \
  -define png:exclude-chunks=date,time PNG24:ui/public/apple-touch-icon.png
convert "$EMBLEM" -colorspace sRGB -filter Lanczos -resize 384x384 \
  -gravity center -background '#0d1117' -extent 512x512 -alpha remove -alpha off \
  -strip -define png:exclude-chunks=date,time PNG24:ui/public/icon-maskable-512.png

# Show the complete supplied logo, without cropping its wordmark or tagline.
convert assets/branding/shadowcode-original.png -colorspace sRGB -filter Lanczos \
  -resize 640x640 -gravity center -background '#080c11' -extent 1280x640 \
  -strip -define png:exclude-chunks=date,time PNG24:assets/github-social-preview.png
