# linuxdeploy GTK plugin

`linuxdeploy-plugin-gtk.sh` is linuxdeploy's GTK plugin, copied unchanged from
[linuxdeploy/linuxdeploy-plugin-gtk](https://github.com/linuxdeploy/linuxdeploy-plugin-gtk)
at commit `7a3fbc31a9e5075073ff8790f26effbac5f84453` (MIT; the license text is
pinned in [licenses/native](../../licenses/native/README.md)).

`scripts/build-native.mjs` checks its SHA-256
(`b0f4cbc684a0103a9651f0955b635eaea0096b3a66c0f5a2c2aa337960375171`) and places
it in `target/.tauri/` before the AppImage bundle step, so Tauri never
downloads the plugin from upstream master. To move it, copy the script from a
newer upstream commit, update the digest in `scripts/native-packaging-env.mjs`
and the pinned license in `licenses/native/sources.json` together.
