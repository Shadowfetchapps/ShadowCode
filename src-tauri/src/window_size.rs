//! The main window's size: what the window-state plugin remembers between
//! launches, and the first size, fitted to the screen the window opens on.

use serde::Deserialize;
use std::collections::HashMap;
use tauri::{plugin::TauriPlugin, utils::config::WindowConfig, App, Manager, Monitor, Runtime};
use tauri_plugin_window_state::StateFlags;

/// What the window-state plugin saves and restores: maximized, full screen,
/// visibility, decorations and position, but not the size. A Wayland
/// compositor places windows itself, so the position only counts on X11.
///
/// On Wayland the window draws its own title bar (a GTK header bar). The
/// plugin saved the size GTK reports in its configure event, which includes
/// that header bar and the shadow margins, and restoring it added them
/// again, so the window grew on every launch (about 52 px wide and 99 px
/// tall). The window now opens at its configured size fitted to the screen
/// ([`fit_config`]) on every desktop, and a size saved by an earlier version
/// is ignored.
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

/// Height of the window manager's title bar on X11, which is outside the
/// window's inner size (KWin and Mutter draw 30 to 40 px).
const X11_TITLE_BAR: f64 = 48.0;

/// The kind of session GTK opened the app in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Desktop {
    /// The window draws its own title bar (a GTK header bar) inside the size
    /// it asks for, and the compositor chooses where the window opens.
    Wayland,
    /// The window manager adds its title bar outside the window's size, and
    /// the window opens where Tauri or the window-state plugin puts it.
    X11,
}

impl Desktop {
    /// The session GTK opened, which is also how tao decides to draw a header
    /// bar. The AppImage runs on X11 (`GDK_BACKEND=x11`) even in a Wayland
    /// session.
    pub fn current() -> Self {
        #[cfg(target_os = "linux")]
        {
            use gdk::prelude::DisplayExtManual;
            if gdk::Display::default().is_some_and(|display| display.backend().is_wayland()) {
                return Self::Wayland;
            }
        }
        Self::X11
    }

    /// Room the title bar takes outside the window's inner size.
    fn title_bar(self) -> f64 {
        match self {
            Self::Wayland => 0.0,
            Self::X11 => X11_TITLE_BAR,
        }
    }
}

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
        let scale = valid_scale(scale);
        let logical = |(w, h): (u32, u32)| (f64::from(w) / scale, f64::from(h) / scale);
        Self {
            size: logical(size),
            work_area: logical(work_area),
        }
    }

    fn usable(&self) -> bool {
        let area = self.size.0 * self.size.1;
        area.is_finite() && area > 0.0
    }
}

fn valid_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// A rectangle in physical pixels, the unit the window-state plugin saves
/// positions in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

impl Rect {
    fn contains(&self, (x, y): (i64, i64)) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// A monitor: where it is on the desktop and its work area, in physical
/// pixels, and its scale factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub area: Rect,
    pub work_area: Rect,
    pub scale: f64,
}

impl Placed {
    fn of(monitor: &Monitor) -> Self {
        let (position, size, work) = (monitor.position(), monitor.size(), monitor.work_area());
        Self {
            area: Rect {
                x: position.x.into(),
                y: position.y.into(),
                width: size.width.into(),
                height: size.height.into(),
            },
            work_area: Rect {
                x: work.position.x.into(),
                y: work.position.y.into(),
                width: work.size.width.into(),
                height: work.size.height.into(),
            },
            scale: monitor.scale_factor(),
        }
    }

    /// The monitor as a screen in logical pixels.
    pub fn screen(&self) -> Screen {
        let size = |rect: Rect| {
            (
                u32::try_from(rect.width).unwrap_or(0),
                u32::try_from(rect.height).unwrap_or(0),
            )
        };
        Screen::logical(size(self.area), size(self.work_area), self.scale)
    }
}

/// The window-state plugin's record of a window, as its `restore_state`
/// reads it (tauri-plugin-window-state 2.4.1 keeps the type private). A file
/// with a record that lacks a field is not read at all, by the plugin or
/// here.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub struct Saved {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub prev_x: i32,
    pub prev_y: i32,
    pub maximized: bool,
    pub visible: bool,
    pub decorated: bool,
    pub fullscreen: bool,
}

impl Saved {
    /// The plugin's default record, which it never restores.
    const UNTOUCHED: Self = Self {
        width: 0,
        height: 0,
        x: 0,
        y: 0,
        prev_x: 0,
        prev_y: 0,
        maximized: false,
        visible: true,
        decorated: true,
        fullscreen: false,
    };

    /// The main window's record in the plugin's file.
    pub fn main_window(file: &str) -> Option<Self> {
        serde_json::from_str::<HashMap<String, Self>>(file)
            .ok()?
            .remove("main")
    }

    /// Where the plugin moves the window back to, and the monitor there. Like
    /// the plugin: only when a corner of the saved rectangle is on a monitor,
    /// and to the place before maximizing for a maximized window. A size
    /// saved by 1.0.0 still takes part in that check (the plugin keeps it,
    /// unused).
    fn restores_to(&self, monitors: &[Placed]) -> Option<((i64, i64), Placed)> {
        if *self == Self::UNTOUCHED {
            return None;
        }
        let (x, y) = (i64::from(self.x), i64::from(self.y));
        let (right, bottom) = (x + i64::from(self.width), y + i64::from(self.height));
        let corners = [(x, y), (right, y), (x, bottom), (right, bottom)];
        let touched = monitors
            .iter()
            .find(|monitor| corners.iter().any(|&corner| monitor.area.contains(corner)))?;
        let position = if self.maximized {
            (i64::from(self.prev_x), i64::from(self.prev_y))
        } else {
            (x, y)
        };
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.area.contains(position))
            .unwrap_or(touched);
        Some((position, *monitor))
    }
}

/// Where the main window opens: the screens to fit it to, and on X11 the
/// place the window-state plugin moves it back to.
#[derive(Clone, Debug, PartialEq)]
pub struct Opening {
    pub desktop: Desktop,
    /// The screens the window may open on. It fits each of them, and opens
    /// maximized only when none of them holds its minimum size.
    pub screens: Vec<Screen>,
    /// The plugin's move back to where a window that was not maximized or
    /// full screen was (X11 only).
    pub restored: Option<Restored>,
}

/// A position the window-state plugin restores, and the monitor there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Restored {
    pub position: (i64, i64),
    pub monitor: Placed,
}

/// Choose the screens the window opens on.
///
/// On X11 that is the monitor the window-state plugin moves the window back
/// to, which can be a smaller screen than the primary one. Without a saved
/// place it is the primary monitor, where Tauri centers a new window. On
/// Wayland the compositor places the window (and ignores the plugin's
/// position), so the window fits every screen; so does an X11 desktop with
/// no primary monitor.
pub fn opening(
    desktop: Desktop,
    monitors: &[Placed],
    primary: Option<Placed>,
    saved: Option<Saved>,
) -> Option<Opening> {
    let monitors: Vec<Placed> = monitors
        .iter()
        .copied()
        .filter(|monitor| monitor.screen().usable())
        .collect();
    let on = |monitor: Placed, restored| Opening {
        desktop,
        screens: vec![monitor.screen()],
        restored,
    };
    if desktop == Desktop::X11 {
        if let Some(saved) = saved {
            if let Some((position, monitor)) = saved.restores_to(&monitors) {
                let restored = (!saved.maximized && !saved.fullscreen)
                    .then_some(Restored { position, monitor });
                return Some(on(monitor, restored));
            }
        }
        if let Some(primary) = primary.filter(|primary| primary.screen().usable()) {
            return Some(on(primary, None));
        }
    }
    let screens: Vec<Screen> = monitors.iter().map(Placed::screen).collect();
    (!screens.is_empty()).then_some(Opening {
        desktop,
        screens,
        restored: None,
    })
}

/// The screens of this session (see [`opening`]). `restores_state` is false
/// for an isolated profile, which has no window-state plugin.
pub fn opening_on<R: Runtime>(app: &App<R>, restores_state: bool) -> Option<Opening> {
    let monitors: Vec<Placed> = app
        .available_monitors()
        .map(|monitors| monitors.iter().map(Placed::of).collect())
        .unwrap_or_default();
    let primary = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| Placed::of(&monitor));
    let saved = restores_state
        .then(|| app.path().app_config_dir().ok())
        .flatten()
        .and_then(|dir| {
            std::fs::read_to_string(dir.join(tauri_plugin_window_state::DEFAULT_FILENAME)).ok()
        })
        .and_then(|file| Saved::main_window(&file));
    opening(Desktop::current(), &monitors, primary, saved)
}

/// The window's first inner size, and whether it opens maximized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fitted {
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
}

/// Fit the configured inner `size` to `screen`: at most 90% of the screen
/// (and never more than its work area), less `title_bar` for a title bar
/// outside the inner size. When even the minimum size does not fit, the
/// window keeps the minimum and opens maximized, so the window manager fits
/// it to the screen.
pub fn fit(size: (f64, f64), min: (f64, f64), screen: Screen, title_bar: f64) -> Fitted {
    let room = |full: f64, work: f64| {
        let share = full * SCREEN_SHARE;
        if work > 0.0 {
            share.min(work)
        } else {
            share
        }
    };
    let room_width = room(screen.size.0, screen.work_area.0).floor();
    let room_height = (room(screen.size.1, screen.work_area.1) - title_bar).floor();
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

/// Fit to every screen the window may open on: the smallest width and
/// height any of them allows, maximized only when none of them holds the
/// minimum.
pub fn fit_all(size: (f64, f64), min: (f64, f64), opening: &Opening) -> Option<Fitted> {
    let title_bar = opening.desktop.title_bar();
    opening
        .screens
        .iter()
        .map(|&screen| fit(size, min, screen, title_bar))
        .reduce(|a, b| Fitted {
            width: a.width.min(b.width),
            height: a.height.min(b.height),
            maximized: a.maximized && b.maximized,
        })
}

/// Fit a window configuration to the screens it opens on, before the window
/// is built.
///
/// Only the initial size changes. A maximized state the window-state plugin
/// restores is applied after the window exists and is kept; un-maximizing
/// then returns to this size. A configuration that asks for a maximized
/// window stays maximized.
pub fn fit_config(config: &mut WindowConfig, opening: &Opening) {
    let min = (
        config.min_width.unwrap_or(0.0),
        config.min_height.unwrap_or(0.0),
    );
    let Some(fitted) = fit_all((config.width, config.height), min, opening) else {
        return;
    };
    config.width = fitted.width;
    config.height = fitted.height;
    config.maximized |= fitted.maximized;
}

impl Opening {
    /// Where to move a window of inner `size` (logical) after the
    /// window-state plugin put it back where it was, so that it and its title
    /// bar stay inside that monitor's work area: a window last left low on
    /// the screen would otherwise open with its status bar under the panel.
    /// None when it already fits there, or nothing is restored.
    pub fn inside_work_area(&self, size: (f64, f64)) -> Option<(i64, i64)> {
        let Restored {
            position: (x, y),
            monitor,
        } = self.restored?;
        let scale = valid_scale(monitor.scale);
        let width = (size.0 * scale).ceil() as i64;
        let height = ((size.1 + self.desktop.title_bar()) * scale).ceil() as i64;
        let work = monitor.work_area;
        // Not `clamp`: a window larger than the work area keeps its top left
        // corner inside it.
        let fit =
            |at: i64, length: i64, start: i64, room: i64| at.min(start + room - length).max(start);
        let inside = (
            fit(x, width, work.x, work.width),
            fit(y, height, work.y, work.height),
        );
        (inside != (x, y)).then_some(inside)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: (f64, f64) = (1380.0, 920.0);
    const MIN: (f64, f64) = (520.0, 640.0);
    /// Height of the GTK header bar on Wayland with Plasma's Breeze theme
    /// (Shadowfetch Linux 5.0 QA), and of Plasma's default panel.
    const HEADER_BAR: f64 = 47.0;
    const PANEL: f64 = 46.0;

    /// A Wayland screen: the work area is the whole screen.
    fn wayland(width: f64, height: f64) -> Screen {
        Screen {
            size: (width, height),
            work_area: (width, height),
        }
    }

    /// A monitor at `x`,`y` on the desktop, with a `panel` px panel at the
    /// bottom, at scale 1.
    fn monitor(x: i64, y: i64, width: i64, height: i64, panel: i64) -> Placed {
        Placed {
            area: Rect {
                x,
                y,
                width,
                height,
            },
            work_area: Rect {
                x,
                y,
                width,
                height: height - panel,
            },
            scale: 1.0,
        }
    }

    /// A saved record of a window that was not maximized, at `x`,`y`.
    fn saved_at(x: i32, y: i32) -> Saved {
        Saved {
            x,
            y,
            prev_x: x,
            prev_y: y,
            ..Saved::UNTOUCHED
        }
    }

    /// The window configuration shipped in `tauri.conf.json`.
    fn shipped_config() -> WindowConfig {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        serde_json::from_value(conf["app"]["windows"][0].clone()).unwrap()
    }

    fn fitted_on(opening: &Opening) -> Fitted {
        fit_all(DEFAULT, MIN, opening).unwrap()
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
        for title_bar in [0.0, X11_TITLE_BAR] {
            let fitted = fit(DEFAULT, MIN, wayland(1920.0, 1080.0), title_bar);
            assert_eq!(
                fitted,
                Fitted {
                    width: 1380.0,
                    height: 920.0,
                    maximized: false
                }
            );
        }
    }

    #[test]
    fn a_1366x768_wayland_screen_fits_above_its_panel() {
        // On Wayland the header bar is inside the size the window asks for:
        // 1.0.0's 1380x920 window measured 920 px with its header bar on
        // Plasma, and Weston reports the window geometry as the asked size.
        let fitted = fit(
            DEFAULT,
            MIN,
            wayland(1366.0, 768.0),
            Desktop::Wayland.title_bar(),
        );
        assert_eq!(
            fitted,
            Fitted {
                width: 1229.0,
                height: 691.0,
                maximized: false
            }
        );
        // The whole window, header bar included, stays above the panel, and
        // the content below the header bar keeps the minimum height.
        assert!(fitted.height + PANEL <= 768.0);
        assert!(fitted.height - HEADER_BAR >= MIN.1);
        assert!(fitted.width <= 1366.0);
    }

    #[test]
    fn a_1366x768_x11_screen_fits_with_the_window_manager_title_bar() {
        // X11 reports the panel; the window manager's title bar is outside
        // the inner size.
        let screen = Screen {
            size: (1366.0, 768.0),
            work_area: (1366.0, 768.0 - PANEL),
        };
        let fitted = fit(DEFAULT, MIN, screen, Desktop::X11.title_bar());
        assert_eq!(
            fitted,
            Fitted {
                width: 1229.0,
                height: 643.0,
                maximized: false
            }
        );
        assert!(fitted.height + X11_TITLE_BAR <= 768.0 - PANEL);
    }

    #[test]
    fn a_known_work_area_is_respected() {
        // X11 reports panels: a 180 px panel on a 1080 px screen.
        let screen = Screen {
            size: (1920.0, 1080.0),
            work_area: (1920.0, 900.0),
        };
        let fitted = fit(DEFAULT, MIN, screen, X11_TITLE_BAR);
        assert_eq!((fitted.width, fitted.height), (1380.0, 852.0));
        assert!(!fitted.maximized);
    }

    #[test]
    fn a_screen_below_the_minimum_opens_maximized() {
        let fitted = fit(DEFAULT, MIN, wayland(1024.0, 600.0), 0.0);
        assert_eq!(
            fitted,
            Fitted {
                width: 921.0,
                height: 640.0,
                maximized: true
            }
        );
        let narrow = fit(DEFAULT, MIN, wayland(560.0, 1024.0), 0.0);
        assert_eq!((narrow.width, narrow.height), (520.0, 920.0));
        assert!(narrow.maximized);
        // 1280x720 holds the minimum on Wayland (648 px with the header
        // bar), but not below an X11 title bar (600 px).
        assert!(!fit(DEFAULT, MIN, wayland(1280.0, 720.0), 0.0).maximized);
        let x11 = fit(DEFAULT, MIN, wayland(1280.0, 720.0), X11_TITLE_BAR);
        assert_eq!(
            (x11.width, x11.height, x11.maximized),
            (1152.0, 640.0, true)
        );
    }

    #[test]
    fn a_screen_without_a_size_keeps_the_configured_window() {
        let configured = Fitted {
            width: 1380.0,
            height: 920.0,
            maximized: false,
        };
        assert_eq!(fit(DEFAULT, MIN, wayland(0.0, 0.0), 0.0), configured);
        assert_eq!(fit(DEFAULT, MIN, wayland(f64::NAN, 768.0), 0.0), configured);
        assert_eq!(
            fit(DEFAULT, MIN, wayland(1366.0, 40.0), X11_TITLE_BAR),
            configured
        );
    }

    #[test]
    fn scaled_screens_are_measured_in_logical_pixels() {
        // A 2560x1600 panel at 200%: 1280x800 logical.
        let screen = Screen::logical((2560, 1600), (2560, 1600), 2.0);
        assert_eq!(screen, wayland(1280.0, 800.0));
        let fitted = fit(DEFAULT, MIN, screen, 0.0);
        assert_eq!((fitted.width, fitted.height), (1152.0, 720.0));
        assert!(!fitted.maximized);
        // An invalid scale factor counts as 1.
        assert_eq!(
            Screen::logical((1366, 768), (1366, 722), 0.0),
            Screen {
                size: (1366.0, 768.0),
                work_area: (1366.0, 722.0)
            }
        );
        let mut hidpi = monitor(0, 0, 2560, 1600, 0);
        hidpi.scale = 2.0;
        assert_eq!(hidpi.screen(), wayland(1280.0, 800.0));
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
        let laptop = opening(Desktop::Wayland, &[monitor(0, 0, 1366, 768, 0)], None, None).unwrap();
        fit_config(&mut config, &laptop);
        assert_eq!((config.width, config.height), (1229.0, 691.0));
        assert!(!config.maximized);
        assert!(config.center, "still centered, at the fitted size");

        let mut large = shipped_config();
        let desk = opening(Desktop::X11, &[], Some(monitor(0, 0, 2560, 1440, 0)), None).unwrap();
        fit_config(&mut large, &desk);
        assert_eq!((large.width, large.height), DEFAULT);
        assert!(!large.maximized);
    }

    #[test]
    fn fitting_never_undoes_a_maximized_window() {
        let large = opening(
            Desktop::Wayland,
            &[monitor(0, 0, 1920, 1080, 0)],
            None,
            None,
        )
        .unwrap();
        let mut config = shipped_config();
        config.maximized = true;
        fit_config(&mut config, &large);
        assert!(config.maximized);

        let tiny = opening(Desktop::Wayland, &[monitor(0, 0, 1024, 600, 0)], None, None).unwrap();
        let mut small = shipped_config();
        fit_config(&mut small, &tiny);
        assert!(small.maximized);
        assert_eq!((small.width, small.height), (921.0, 640.0));
    }

    #[test]
    fn wayland_fits_every_screen_and_maximizes_only_when_none_holds_the_minimum() {
        let laptop = monitor(0, 0, 1366, 768, 0);
        let external = monitor(1366, 0, 2560, 1440, 0);
        // The compositor picks the screen: the window fits either one.
        let both = opening(Desktop::Wayland, &[external, laptop], None, None).unwrap();
        assert_eq!(both.screens, vec![external.screen(), laptop.screen()]);
        assert_eq!(
            fitted_on(&both),
            Fitted {
                width: 1229.0,
                height: 691.0,
                maximized: false
            }
        );
        // A 1280x720 logical screen (2560x1440 at 200%) beside a large one
        // does not force a maximized window, which the user could not undo
        // (the plugin only ever maximizes).
        let mut scaled = monitor(0, 0, 2560, 1440, 0);
        scaled.scale = 2.0;
        let pair = opening(Desktop::Wayland, &[scaled, external], None, None).unwrap();
        assert!(!fitted_on(&pair).maximized);
        let mut config = shipped_config();
        fit_config(&mut config, &pair);
        assert!(!config.maximized);
        // Neither does a screen below the minimum, as long as another one
        // holds it: the window keeps the minimum height.
        let netbook = monitor(0, 0, 1024, 600, 0);
        let mixed = opening(Desktop::Wayland, &[netbook, external], None, None).unwrap();
        assert_eq!(
            fitted_on(&mixed),
            Fitted {
                width: 921.0,
                height: 640.0,
                maximized: false
            }
        );
        // Only small screens: maximized.
        let small = opening(Desktop::Wayland, &[netbook, netbook], None, None).unwrap();
        assert!(fitted_on(&small).maximized);
        // A primary monitor or a saved place does not count on Wayland.
        let ignored = opening(
            Desktop::Wayland,
            &[laptop, external],
            Some(external),
            Some(saved_at(1500, 100)),
        )
        .unwrap();
        assert_eq!(ignored.screens.len(), 2);
        assert_eq!(ignored.restored, None);
    }

    #[test]
    fn x11_fits_the_screen_the_window_is_moved_back_to() {
        // A docked laptop: a 2560x1440 primary screen and the 1366x768 laptop
        // panel to its right, where ShadowCode was last closed.
        let external = monitor(0, 0, 2560, 1440, 0);
        let laptop = monitor(2560, 0, 1366, 768, 46);
        let monitors = [external, laptop];
        let back = opening(
            Desktop::X11,
            &monitors,
            Some(external),
            Some(saved_at(2600, 10)),
        )
        .unwrap();
        assert_eq!(back.screens, vec![laptop.screen()]);
        assert_eq!(
            back.restored,
            Some(Restored {
                position: (2600, 10),
                monitor: laptop
            })
        );
        assert_eq!(
            fitted_on(&back),
            Fitted {
                width: 1229.0,
                height: 643.0,
                maximized: false
            }
        );
        // Closed on the primary screen: the full size.
        let primary = opening(
            Desktop::X11,
            &monitors,
            Some(external),
            Some(saved_at(200, 100)),
        )
        .unwrap();
        assert_eq!(primary.screens, vec![external.screen()]);
        let full = fitted_on(&primary);
        assert_eq!((full.width, full.height), DEFAULT);
        // Maximized on the laptop panel: the place before maximizing counts,
        // and nothing is moved afterwards.
        let maximized = Saved {
            x: 2560,
            y: 0,
            prev_x: 2700,
            prev_y: 50,
            maximized: true,
            ..Saved::UNTOUCHED
        };
        let max = opening(Desktop::X11, &monitors, Some(external), Some(maximized)).unwrap();
        assert_eq!(max.screens, vec![laptop.screen()]);
        assert_eq!(max.restored, None);
    }

    #[test]
    fn x11_without_a_saved_place_uses_the_primary_screen() {
        let external = monitor(0, 0, 2560, 1440, 0);
        let laptop = monitor(2560, 0, 1366, 768, 46);
        let monitors = [external, laptop];
        // No file, the plugin's untouched record, a place off every screen
        // (the plugin leaves the window where Tauri put it): the primary.
        for saved in [None, Some(Saved::UNTOUCHED), Some(saved_at(-5000, -5000))] {
            let opened = opening(Desktop::X11, &monitors, Some(laptop), saved).unwrap();
            assert_eq!(opened.screens, vec![laptop.screen()]);
            assert_eq!(opened.restored, None);
        }
        // No primary monitor either: the window fits every screen.
        let anywhere = opening(Desktop::X11, &monitors, None, None).unwrap();
        assert_eq!(anywhere.screens, vec![external.screen(), laptop.screen()]);
        // No screens at all: the configured window.
        assert_eq!(opening(Desktop::X11, &[], None, None), None);
        assert_eq!(
            opening(Desktop::Wayland, &[monitor(0, 0, 0, 0, 0)], None, None),
            None
        );
    }

    #[test]
    fn a_1_0_0_size_still_decides_whether_the_place_is_restored() {
        let laptop = monitor(0, 0, 1366, 768, 46);
        // The plugin checks only the corners of the saved rectangle: a grown
        // 1484x1071 window around the whole screen is not moved back.
        let grown = Saved {
            width: 1484,
            height: 1071,
            x: -50,
            y: -60,
            prev_x: -50,
            prev_y: -60,
            ..Saved::UNTOUCHED
        };
        let left = opening(Desktop::X11, &[laptop], None, Some(grown)).unwrap();
        assert_eq!(left.screens, vec![laptop.screen()]);
        assert_eq!(left.restored, None);
        // A corner on the screen: moved back, even with its top left corner
        // above the screen, and then moved inside the work area.
        let corner = Saved {
            width: 1000,
            height: 700,
            ..grown
        };
        let back = opening(Desktop::X11, &[laptop], None, Some(corner)).unwrap();
        assert_eq!(back.restored.map(|r| r.position), Some((-50, -60)));
        assert_eq!(back.inside_work_area((1229.0, 643.0)), Some((0, 0)));
    }

    #[test]
    fn a_window_moved_back_stays_inside_the_work_area() {
        let external = monitor(0, 0, 2560, 1440, 0);
        let laptop = monitor(2560, 0, 1366, 768, 46);
        let low = opening(
            Desktop::X11,
            &[external, laptop],
            Some(external),
            Some(saved_at(2700, 300)),
        )
        .unwrap();
        // 1229x643 plus a 48 px title bar from y=300 would end at 991, under
        // the panel at 722: it moves up and left to fit.
        let size = (1229.0, 643.0);
        assert_eq!(
            low.inside_work_area(size),
            Some((2560 + 1366 - 1229, 722 - 691))
        );
        // A window that already fits stays where it was.
        let fits = opening(
            Desktop::X11,
            &[external, laptop],
            Some(external),
            Some(saved_at(2600, 10)),
        )
        .unwrap();
        assert_eq!(fits.inside_work_area(size), None);
        // Nothing restored, nothing moved.
        let fresh = opening(Desktop::X11, &[external, laptop], Some(external), None).unwrap();
        assert_eq!(fresh.inside_work_area(size), None);
        // At 200%, in physical pixels: 1229x691 logical is 2458x1382.
        let mut hidpi = monitor(0, 0, 2732, 1536, 92);
        hidpi.scale = 2.0;
        let scaled = opening(
            Desktop::X11,
            &[hidpi],
            Some(hidpi),
            Some(saved_at(200, 200)),
        )
        .unwrap();
        assert_eq!(scaled.inside_work_area(size), Some((200, 1444 - 1382)));
    }

    #[test]
    fn the_plugin_file_is_read_like_the_plugin_reads_it() {
        let record = r#"{"width":1484,"height":1071,"x":0,"y":0,"prev_x":0,"prev_y":0,"maximized":false,"visible":true,"decorated":true,"fullscreen":false}"#;
        assert_eq!(
            Saved::main_window(&format!(r#"{{"main":{record}}}"#)),
            Some(Saved {
                width: 1484,
                height: 1071,
                ..Saved::UNTOUCHED
            })
        );
        // A record without every field: the plugin reads nothing.
        assert_eq!(Saved::main_window(r#"{"main":{"x":10,"y":10}}"#), None);
        assert_eq!(
            Saved::main_window(&format!(r#"{{"other":{record}}}"#)),
            None
        );
        assert_eq!(Saved::main_window("not json"), None);
    }
}
