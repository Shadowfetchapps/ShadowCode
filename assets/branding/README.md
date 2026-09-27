# ShadowCode logo provenance

The supplied final logo reads **SHADOWCODE** and “ONE HARNESS. ALL MODELS.”
It supersedes the earlier ShadowfetchCode artwork. The download filename does
not change the product name, command names, application IDs, or storage paths.

- `shadowcode-original.png` is the exact, unchanged final file supplied as
  `ShadowfetchCodeLogo.png` (1254 × 1254, opaque PNG).
  SHA-256: `653b36e0273c06dfc9ad36489954a6f9dc9ea1aa27fbf1d40195e4a06686920f`.
- `shadowcode-emblem.png` is the selected built-in image generation extraction
  from that final file (1254 × 1254, PNG with real alpha transparency). Original
  generated filename: `exec-aa53c26a-bed2-47bf-9417-28eaa1247301.png`.
  SHA-256: `3bdda4e1d0e8a7149d3aea358e08eea9d3338f345604c0da0344ae3f8a3fc573`.

The image generation call used the final original as its reference and
`transparent_background=true`, with this prompt:

> Use case: background-extraction. This image is the user's FINAL replacement
> ShadowCode logo, and supersedes the earlier ShadowfetchCode art. Extract ONLY
> the exact central gold-left/dark-steel-right hexagonal S emblem from this image
> into a square app icon with real alpha transparency. Remove the background,
> floor reflection, SHADOWCODE wordmark and tagline. Preserve this version's
> precise geometry, bevels, proportions, metallic texture and gold illumination;
> do not redesign or add any elements. Internal dark surfaces of the emblem stay
> dark. Center the whole emblem uncropped with about 8% transparent padding around
> its extent. No text, extra outline, badge or checkerboard raster. High-fidelity
> background cutout of this specific provided logo.

Run `bash assets/branding/generate-icons.sh` from any directory to regenerate
the desktop sizes, hicolor tree, web/PWA icons, Apple icon, favicon, SVG
compatibility wrappers, and social preview. The script verifies source hashes
before writing derivatives. It uses ImageMagick `convert` (generated with
6.9.12-98 Q16), Lanczos resizing, fixed colors, and stripped metadata. It does
not rebuild `ui/dist`, install the app, or alter either source PNG.

Native and browser icons use the emblem without the tiny wordmark. Transparent
PNG variants preserve its interior dark surfaces and its original padding.
Apple and maskable icons have an opaque charcoal background; the maskable
variant adds safe cropping space. Legacy SVG paths embed the 256 px PNG, with
no external dependencies; they are raster compatibility wrappers, not newly
drawn vector artwork. The README and social preview show the complete original.
