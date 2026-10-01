//! The main window's size: what the window-state plugin remembers between
//! launches.

use tauri::{plugin::TauriPlugin, Runtime};
use tauri_plugin_window_state::StateFlags;

/// What the window-state plugin saves and restores: position, maximized,
/// visibility, decorations and full screen, but not the size.
///
/// On Wayland the window draws its own title bar (a GTK header bar). The
/// plugin saved the size GTK reports in its configure event, which includes
/// that header bar and the shadow margins, and restoring it added them
/// again, so the window grew on every launch (about 52 px wide and 99 px
/// tall). The window now opens at its configured size, and a size saved by
/// an earlier version is ignored.
pub const REMEMBERED: StateFlags = StateFlags::all().difference(StateFlags::SIZE);

/// The window-state plugin, remembering [`REMEMBERED`].
pub fn state_plugin<R: Runtime>() -> TauriPlugin<R> {
    tauri_plugin_window_state::Builder::default()
        .with_state_flags(REMEMBERED)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_everything_but_the_size() {
        assert!(!REMEMBERED.contains(StateFlags::SIZE));
        assert!(REMEMBERED.contains(
            StateFlags::MAXIMIZED
                | StateFlags::POSITION
                | StateFlags::VISIBLE
                | StateFlags::DECORATIONS
                | StateFlags::FULLSCREEN
        ));
    }
}
