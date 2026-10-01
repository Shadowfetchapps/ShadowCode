//! The main window's size: what the window-state plugin remembers between
//! launches, and the first size on a screen smaller than the configured one.

use tauri::{plugin::TauriPlugin, utils::config::WindowConfig, App, Monitor, Runtime};
use tauri_plugin_window_state::StateFlags;

/// What the window-state plugin saves and restores: position, maximized,
/// visibility, decorations and full screen, but not the size.
///
/// On Wayland the window draws its own title bar (a GTK header bar). The
/// plugin saved the size GTK reports in its configure event, which includes
/// that header bar and the shadow margins, and restoring it added them
/// again, so the window grew on every launch (about 52 px wide and 99 px
/// tall). The window now opens at its configured size fitted to the screen
/// ([`fit_config`]), and a size saved by an earlier version is ignored.
pub const REMEMBERED: StateFlags = StateFlags::all().difference(StateFlags::SIZE);

/// The window-state plugin, remembering [`REMEMBERED`].
pub fn state_plugin<R: Runtime>() -> TauriPlugin<R> {
    tauri_plugin_window_state::Builder::default()
        .with_state_flags(REMEMBERED)
        .build()
}

/// Share of the screen's width and height the first window covers at most.
/// It leaves room for panels the toolkit does not report: on Wayland the
/// work area is the whole screen.
const SCREEN_SHARE: f64 = 0.9;

/// Height of the title bar, which is not part of the window's inner size:
/// GTK's header bar on Wayland (46 px with Plasma's Breeze theme) or the
/// window manager's frame on X11.
const TITLE_BAR: f64 = 48.0;

/// A screen in logical pixels: its whole size and the part windows may use
/// (without panels, where the system says so).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen {
    pub size: (f64, f64),
    pub work_area: (f64, f64),
}

impl Screen {
    /// From physical pixels and the screen's scale factor.
    pub fn logical(size: (u32, u32), work_area: (u32, u32), scale: f64) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let logical = |(w, h): (u32, u32)| (f64::from(w) / scale, f64::from(h) / scale);
        Self {
            size: logical(size),
            work_area: logical(work_area),
        }
    }

    fn of(monitor: &Monitor) -> Self {
        let size = monitor.size();
        let work_area = monitor.work_area().size;
        Self::logical(
            (size.width, size.height),
            (work_area.width, work_area.height),
            monitor.scale_factor(),
        )
    }

    fn area(&self) -> f64 {
        self.size.0 * self.size.1
    }
}

/// The window's first inner size, and whether it opens maximized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fitted {
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
}

/// Fit the configured inner `size` to `screen`: at most 90% of the screen
/// (and never more than its work area), with room for the title bar. When
/// even the minimum size does not fit, the window keeps the minimum and
/// opens maximized, so the window manager fits it to the screen.
pub fn fit(size: (f64, f64), min: (f64, f64), screen: Screen) -> Fitted {
    let room = |full: f64, work: f64| {
        let share = full * SCREEN_SHARE;
        if work > 0.0 {
            share.min(work)
        } else {
            share
        }
    };
    let room_width = room(screen.size.0, screen.work_area.0).floor();
    let room_height = (room(screen.size.1, screen.work_area.1) - TITLE_BAR).floor();
    // A screen that reports no usable size: keep the configured window.
    let usable = |length: f64| length.is_finite() && length > 0.0;
    if !(usable(room_width) && usable(room_height)) {
        return Fitted {
            width: size.0,
            height: size.1,
            maximized: false,
        };
    }
    let width = size.0.min(room_width);
    let height = size.1.min(room_height);
    Fitted {
        width: width.max(min.0),
        height: height.max(min.1),
        maximized: width < min.0 || height < min.1,
    }
}

/// Fit a window configuration to `screen` before the window is built.
///
/// Only the initial size changes. A maximized state the window-state plugin
/// restores is applied after the window exists and is kept; un-maximizing
/// then returns to this size. A configuration that asks for a maximized
/// window stays maximized.
pub fn fit_config(config: &mut WindowConfig, screen: Screen) {
    let fitted = fit(
        (config.width, config.height),
        (
            config.min_width.unwrap_or(0.0),
            config.min_height.unwrap_or(0.0),
        ),
        screen,
    );
    config.width = fitted.width;
    config.height = fitted.height;
    config.maximized |= fitted.maximized;
}

/// The screen the window opens on: the primary monitor, where Tauri centers
/// a new window. Wayland names no primary monitor and lets the compositor
/// choose, so there it is the smallest screen: the window then fits
/// wherever it appears.
pub fn opening_screen<R: Runtime>(app: &App<R>) -> Option<Screen> {
    if let Ok(Some(primary)) = app.primary_monitor() {
        return Some(Screen::of(&primary));
    }
    smallest(app.available_monitors().ok()?.iter().map(Screen::of))
}

fn smallest(screens: impl IntoIterator<Item = Screen>) -> Option<Screen> {
    screens
        .into_iter()
        .filter(|screen| screen.area() > 0.0)
        .min_by(|a, b| a.area().total_cmp(&b.area()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: (f64, f64) = (1380.0, 920.0);
    const MIN: (f64, f64) = (520.0, 640.0);

    /// A Wayland screen: the work area is the whole screen.
    fn wayland(width: f64, height: f64) -> Screen {
        Screen {
            size: (width, height),
            work_area: (width, height),
        }
    }

    /// The window configuration shipped in `tauri.conf.json`.
    fn shipped_config() -> WindowConfig {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        serde_json::from_value(conf["app"]["windows"][0].clone()).unwrap()
    }

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

    #[test]
    fn a_large_screen_keeps_the_configured_size() {
        let fitted = fit(DEFAULT, MIN, wayland(1920.0, 1080.0));
        assert_eq!(
            fitted,
            Fitted {
                width: 1380.0,
                height: 920.0,
                maximized: false
            }
        );
    }

    #[test]
    fn a_1366x768_screen_fits_above_its_panel() {
        let fitted = fit(DEFAULT, MIN, wayland(1366.0, 768.0));
        assert_eq!(
            fitted,
            Fitted {
                width: 1229.0,
                height: 643.0,
                maximized: false
            }
        );
        // Plasma's 46 px panel and a 46 px header bar still leave the whole
        // window, composer and status bar included, on the screen.
        assert!(fitted.height + 46.0 + 46.0 <= 768.0);
        assert!(fitted.width <= 1366.0);
    }

    #[test]
    fn a_known_work_area_is_respected() {
        // X11 reports panels: a 180 px panel on a 1080 px screen.
        let screen = Screen {
            size: (1920.0, 1080.0),
            work_area: (1920.0, 900.0),
        };
        let fitted = fit(DEFAULT, MIN, screen);
        assert_eq!((fitted.width, fitted.height), (1380.0, 852.0));
        assert!(!fitted.maximized);
    }

    #[test]
    fn a_screen_below_the_minimum_opens_maximized() {
        let fitted = fit(DEFAULT, MIN, wayland(1280.0, 720.0));
        assert_eq!(
            fitted,
            Fitted {
                width: 1152.0,
                height: 640.0,
                maximized: true
            }
        );
        let narrow = fit(DEFAULT, MIN, wayland(560.0, 1024.0));
        assert_eq!((narrow.width, narrow.height), (520.0, 873.0));
        assert!(narrow.maximized);
    }

    #[test]
    fn a_screen_without_a_size_keeps_the_configured_window() {
        let configured = Fitted {
            width: 1380.0,
            height: 920.0,
            maximized: false,
        };
        assert_eq!(fit(DEFAULT, MIN, wayland(0.0, 0.0)), configured);
        assert_eq!(fit(DEFAULT, MIN, wayland(f64::NAN, 768.0)), configured);
        assert_eq!(fit(DEFAULT, MIN, wayland(1366.0, 40.0)), configured);
    }

    #[test]
    fn scaled_screens_are_measured_in_logical_pixels() {
        // A 2560x1600 panel at 200%: 1280x800 logical.
        let screen = Screen::logical((2560, 1600), (2560, 1600), 2.0);
        assert_eq!(screen, wayland(1280.0, 800.0));
        let fitted = fit(DEFAULT, MIN, screen);
        assert_eq!((fitted.width, fitted.height), (1152.0, 672.0));
        assert!(!fitted.maximized);
        // An invalid scale factor counts as 1.
        assert_eq!(
            Screen::logical((1366, 768), (1366, 722), 0.0),
            Screen {
                size: (1366.0, 768.0),
                work_area: (1366.0, 722.0)
            }
        );
    }

    #[test]
    fn the_shipped_window_fits_a_1366x768_screen() {
        let mut config = shipped_config();
        assert_eq!(config.label, "main");
        assert!(!config.create, "the main window is built in code");
        assert_eq!((config.width, config.height), DEFAULT);
        assert_eq!(
            (config.min_width, config.min_height),
            (Some(MIN.0), Some(MIN.1))
        );
        fit_config(&mut config, wayland(1366.0, 768.0));
        assert_eq!((config.width, config.height), (1229.0, 643.0));
        assert!(!config.maximized);
        assert!(config.center, "still centered, at the fitted size");

        let mut large = shipped_config();
        fit_config(&mut large, wayland(2560.0, 1440.0));
        assert_eq!((large.width, large.height), DEFAULT);
        assert!(!large.maximized);
    }

    #[test]
    fn fitting_never_undoes_a_maximized_window() {
        let mut config = shipped_config();
        config.maximized = true;
        fit_config(&mut config, wayland(1920.0, 1080.0));
        assert!(config.maximized);

        let mut small = shipped_config();
        fit_config(&mut small, wayland(1024.0, 600.0));
        assert!(small.maximized);
        assert_eq!((small.width, small.height), (921.0, 640.0));
    }

    #[test]
    fn without_a_primary_screen_the_smallest_one_counts() {
        let laptop = wayland(1366.0, 768.0);
        let external = wayland(2560.0, 1440.0);
        assert_eq!(smallest([external, laptop]), Some(laptop));
        assert_eq!(smallest([wayland(0.0, 0.0), external]), Some(external));
        assert_eq!(smallest(Vec::<Screen>::new()), None);
    }
}
