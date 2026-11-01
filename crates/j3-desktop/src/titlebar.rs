//! On macOS the window has no titlebar of its own: the system one is hidden
//! (`appears_transparent`) and the top row of the panes (the sidebar header
//! next to the open thread's header) is the titlebar. The traffic lights sit
//! over the left end of that row, centred on it. AppKit still drags the
//! window from the top of the row (its own titlebar's height); a double click
//! on the row is passed to it here, since GPUI takes the mouse. Elsewhere the
//! headers are only headers.

use gpui_kit::{App, MouseDownEvent, Pixels, Point, Window, point, px};

/// Height of every pane's header row, in rems. The rows line up across the
/// window, so on macOS together they read as one titlebar.
pub const ROW_HEIGHT: f32 = 2.5;

/// Where the close button starts, from the window's left edge.
const TRAFFIC_LIGHTS_X: Pixels = px(14.);

/// AppKit's height for the traffic-light buttons, to centre them on the row.
const TRAFFIC_LIGHT_HEIGHT: Pixels = px(14.);

/// From the window's left edge past the three traffic lights, plus a gap.
const TRAFFIC_LIGHTS_INSET: Pixels = px(80.);

/// The header row's height in this window.
pub fn row_height(window: &Window) -> Pixels {
    window.rem_size() * ROW_HEIGHT
}

/// Where AppKit should put the traffic lights so they sit centred on a
/// header row `row_height` tall.
pub fn traffic_light_position(row_height: Pixels) -> Point<Pixels> {
    point(
        TRAFFIC_LIGHTS_X,
        ((row_height - TRAFFIC_LIGHT_HEIGHT) / 2.).round(),
    )
}

/// The left padding a header at the window's left edge needs to clear the
/// traffic lights; `None` where there are none (other platforms, and full
/// screen, which hides them).
pub fn traffic_lights_inset(window: &Window) -> Option<Pixels> {
    (cfg!(target_os = "macos") && !window.is_fullscreen()).then_some(TRAFFIC_LIGHTS_INSET)
}

/// A double click on the header row that no control in it took (buttons
/// prevent the default) does what the system titlebar's would: zoom or
/// minimize, per System Settings.
pub fn double_click(event: &MouseDownEvent, window: &mut Window, _: &mut App) {
    if event.click_count == 2
        && !window.default_prevented()
        && !window.is_fullscreen()
        && event.position.y < row_height(window)
    {
        window.titlebar_double_click();
    }
}
