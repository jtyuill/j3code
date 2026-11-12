//! The image viewer: a picture from the chat (a message's attachment, a tool's output image,
//! a markdown image) opened over the whole window, where it can be zoomed and panned.
//!
//! It opens fitted to the window (never enlarged past its own size). The wheel and a pinch
//! zoom about the pointer, a trackpad's two-finger scroll pans, a drag pans, a double click
//! switches between the fit and 100%, and the bar at the bottom steps the zoom. Keys: `+`
//! and `-` zoom, `0` fits, `1` is 100%, Escape closes; so does a click beside the picture.
//!
//! Zooming and panning follow the hand, so they land in the frame they happen
//! (`crate::motion`: constant); the viewer itself comes and goes on Rune's fade.

use std::cell::Cell;

use rune::{Color, Cursor, Cx, Drag, El, Image, Key, KeyEvent, Mods, Phase, Tx, el, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant};

use crate::{
    app::{J3, Ui},
    icons,
};

/// Space kept around a fitted picture, in points.
const MARGIN: f32 = 48.0;
/// The largest zoom: points per point of the picture's own size.
const MAX_ZOOM: f32 = 8.0;
/// One step of the zoom buttons and keys.
const STEP: f32 = 1.25;
/// Zoom per point of wheel travel (the exponent's rate), and the most one event counts for.
const WHEEL_RATE: f32 = 0.0025;
const WHEEL_MOST: f32 = 120.0;
/// Pointer travel, in points, under which a press beside the picture is a click that closes.
const CLICK_SLOP: f32 = 4.0;

/// Where the picture sits in the viewer: its zoom and how far its centre is from the
/// view's.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Camera {
    /// Points per point of the picture's own size; `None` fits the view.
    zoom: Option<f32>,
    /// The picture's centre from the view's centre, in points.
    pan: [f32; 2],
}

impl Camera {
    /// The zoom that fits a `natural` picture in `view` with [`MARGIN`] around it, never
    /// above 100%.
    pub fn fit(view: [f32; 2], natural: [f32; 2]) -> f32 {
        let room = |side: f32| (side - 2.0 * MARGIN).max(1.0);
        (room(view[0]) / natural[0].max(1.0))
            .min(room(view[1]) / natural[1].max(1.0))
            .min(1.0)
    }

    /// The zoom drawn.
    pub fn scale(&self, view: [f32; 2], natural: [f32; 2]) -> f32 {
        self.zoom.unwrap_or_else(|| Self::fit(view, natural))
    }

    /// The picture's rect in the view, `[x, y, w, h]`.
    pub fn rect(&self, view: [f32; 2], natural: [f32; 2]) -> [f32; 4] {
        let scale = self.scale(view, natural);
        let (w, h) = (natural[0] * scale, natural[1] * scale);
        [
            view[0] / 2.0 + self.pan[0] - w / 2.0,
            view[1] / 2.0 + self.pan[1] - h / 2.0,
            w,
            h,
        ]
    }

    /// Whether the picture is fitted.
    pub fn fitted(&self) -> bool {
        self.zoom.is_none()
    }

    /// Fits the picture again.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Zooms to `zoom` (kept between the fit and [`MAX_ZOOM`]) holding the picture's point
    /// under `at` (in the view) where it is. Zooming out to the fit fits.
    pub fn zoom_to(&mut self, zoom: f32, at: [f32; 2], view: [f32; 2], natural: [f32; 2]) {
        let fit = Self::fit(view, natural);
        let old = self.scale(view, natural);
        let new = zoom.clamp(fit, MAX_ZOOM.max(fit));
        if new <= fit * 1.001 {
            return self.reset();
        }
        for axis in 0..2 {
            let from_centre = at[axis] - view[axis] / 2.0;
            self.pan[axis] = from_centre - (from_centre - self.pan[axis]) * new / old;
        }
        self.zoom = Some(new);
        self.keep_in_view(view, natural);
    }

    /// Zooms by `factor` about `at`.
    pub fn zoom_by(&mut self, factor: f32, at: [f32; 2], view: [f32; 2], natural: [f32; 2]) {
        self.zoom_to(self.scale(view, natural) * factor, at, view, natural);
    }

    /// Moves the picture so its centre is `pan` from the view's.
    pub fn pan_to(&mut self, pan: [f32; 2], view: [f32; 2], natural: [f32; 2]) {
        self.pan = pan;
        self.keep_in_view(view, natural);
    }

    /// A side that fits the view stays centred; a larger one never shows space past its edge.
    fn keep_in_view(&mut self, view: [f32; 2], natural: [f32; 2]) {
        let scale = self.scale(view, natural);
        for axis in 0..2 {
            let most = ((natural[axis] * scale - view[axis]) / 2.0).max(0.0);
            self.pan[axis] = self.pan[axis].clamp(-most, most);
        }
    }
}

/// The open picture.
pub struct Viewer {
    image: Image,
    /// Device pixels per point the picture draws at for its own size (2 for SVGs).
    density: f32,
    label: String,
    pub camera: Camera,
    /// Where the picture was when the drag in progress began.
    grabbed: [f32; 2],
    /// The viewer's size as last drawn, for the handlers.
    view: Cell<[f32; 2]>,
}

impl Viewer {
    /// The picture's own size in points; `None` until it is decoded.
    fn natural(&self) -> Option<[f32; 2]> {
        let [w, h] = self.image.size()?;
        (w > 0 && h > 0).then(|| [w as f32 / self.density, h as f32 / self.density])
    }

    /// The view and the picture's size, once both are known.
    fn frame(&self) -> Option<([f32; 2], [f32; 2])> {
        let view = self.view.get();
        (view[0] > 0.0 && view[1] > 0.0).then_some((view, self.natural()?))
    }

    /// The zoom as a percentage of the picture's own size.
    pub fn percent(&self) -> Option<u32> {
        let (view, natural) = self.frame()?;
        Some((self.camera.scale(view, natural) * 100.0).round() as u32)
    }

    fn zoom_by(&mut self, factor: f32, at: Option<[f32; 2]>) {
        if let Some((view, natural)) = self.frame() {
            let at = at.unwrap_or([view[0] / 2.0, view[1] / 2.0]);
            self.camera.zoom_by(factor, at, view, natural);
        }
    }

    /// 100% about `at` (the view's centre when `None`).
    fn actual_size(&mut self, at: Option<[f32; 2]>) {
        if let Some((view, natural)) = self.frame() {
            let at = at.unwrap_or([view[0] / 2.0, view[1] / 2.0]);
            self.camera.zoom_to(1.0, at, view, natural);
        }
    }

    /// A double click at `at`: 100% from the fit, the fit from anything else.
    fn toggle(&mut self, at: [f32; 2]) {
        if self.camera.fitted() {
            self.actual_size(Some(at));
        } else {
            self.camera.reset();
        }
    }

    fn drag(&mut self, d: &mut Drag) -> bool {
        let Some((view, natural)) = self.frame() else {
            return false;
        };
        match d.phase {
            Phase::Down => {
                self.grabbed = self.camera.pan;
                if d.clicks == 2 {
                    self.toggle([d.x, d.y]);
                }
            }
            Phase::Move => {
                let pan = [
                    self.grabbed[0] + d.x - d.start_x,
                    self.grabbed[1] + d.y - d.start_y,
                ];
                self.camera.pan_to(pan, view, natural);
            }
            Phase::Up if d.cancelled() => self.camera.pan_to(self.grabbed, view, natural),
            Phase::Up => {
                let [x, y, w, h] = self.camera.rect(view, natural);
                let beside = d.x < x || d.x > x + w || d.y < y || d.y > y + h;
                let still = d.dx.abs() < CLICK_SLOP && d.dy.abs() < CLICK_SLOP;
                return beside && still && d.clicks == 1;
            }
        }
        false
    }
}

impl J3 {
    /// Opens `image` in the viewer, fitted. `density` is the device pixels per point it
    /// draws at for its own size (`Images::density`); `label` names it.
    pub fn open_image(&mut self, image: Image, density: f32, label: impl Into<String>) {
        self.viewer = Some(Viewer {
            image,
            density: density.max(f32::MIN_POSITIVE),
            label: label.into(),
            camera: Camera::default(),
            grabbed: [0.0; 2],
            view: Cell::new([0.0; 2]),
        });
    }

    pub fn close_viewer(&mut self) {
        self.viewer = None;
    }

    /// The viewer's keys while it is open: `+`/`=` and `-` zoom, `0` fits, `1` is 100%,
    /// Escape closes. It takes every other key too, so none reaches the window under it.
    pub(crate) fn viewer_key(&mut self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        let Some(viewer) = self.viewer.as_mut() else {
            return false;
        };
        let plain = ev.mods == Mods::default()
            || ev.mods
                == Mods {
                    shift: true,
                    ..Mods::default()
                };
        match &ev.key {
            Key::Escape => self.viewer = None,
            Key::Text(key) if plain => match key.as_str() {
                "+" | "=" => viewer.zoom_by(STEP, None),
                "-" | "_" => viewer.zoom_by(1.0 / STEP, None),
                "0" => viewer.camera.reset(),
                "1" => viewer.actual_size(None),
                _ => {}
            },
            _ => {}
        }
        tx.instant();
        true
    }
}

/// `picture` (an element drawing `image`) opening the viewer on a press.
pub fn opens(picture: El, image: &Image, density: f32, label: &str) -> El {
    let (image, label) = (image.clone(), label.to_owned());
    picture
        .cursor(Cursor::Pointer)
        .on_press(move |s: &mut J3| s.open_image(image.clone(), density, label.clone()))
}

/// Draws the open viewer into the overlay layer.
pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui) {
    let Some(viewer) = &s.viewer else {
        return;
    };
    let k = ui.k;
    // The viewer covers the window, as the workspace does.
    let view = cx.layout_size("workspace").unwrap_or_default();
    viewer.view.set(view);
    let mut stage = el()
        .key("image-viewer")
        .role("dialog")
        .label(viewer.label.clone())
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .fill(Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.86,
        })
        .clip_soft(0.0)
        .instant_changes()
        .modal(false, |s: &mut J3| s.close_viewer())
        .cursor(if viewer.camera.fitted() {
            Cursor::Default
        } else {
            Cursor::Grab
        })
        .on_drag(|s: &mut J3, d: &mut Drag, tx: &mut Tx| {
            tx.instant();
            if s.viewer.as_mut().is_some_and(|viewer| viewer.drag(d)) {
                s.close_viewer();
            }
        })
        .on_wheel(|s: &mut J3, ev: rune::WheelEvent, tx: &mut Tx| {
            let Some(viewer) = s.viewer.as_mut() else {
                return false;
            };
            tx.instant();
            // A trackpad's scroll pans; a wheel, or either with ⌘/Ctrl held, zooms.
            if ev.precise && !ev.mods.cmd && !ev.mods.ctrl {
                if let Some((view, natural)) = viewer.frame() {
                    let pan = viewer.camera.pan;
                    viewer
                        .camera
                        .pan_to([pan[0] + ev.dx, pan[1] + ev.dy], view, natural);
                }
            } else {
                let travel = ev.dy.clamp(-WHEEL_MOST, WHEEL_MOST);
                viewer.zoom_by((travel * WHEEL_RATE).exp(), Some([ev.x, ev.y]));
            }
            true
        })
        .on_magnify(|s: &mut J3, ev: rune::MagnifyEvent, tx: &mut Tx| {
            let Some(viewer) = s.viewer.as_mut() else {
                return false;
            };
            tx.instant();
            viewer.zoom_by((1.0 + ev.delta).max(0.1), Some([ev.x, ev.y]));
            true
        });
    if let Some(natural) = viewer.natural() {
        let [x, y, w, h] = viewer.camera.rect(view, natural);
        stage = stage.child(
            el().key("image-viewer-image")
                .role("image")
                .label(viewer.label.clone())
                .absolute(x, y)
                .size(w, h)
                .pixels(viewer.image.pixels()),
        );
    }
    let icon_button = |icon, label: &str| {
        IconButton::new(icon)
            .variant(IconButtonVariant::GhostMuted)
            .control_size(ControlSize::Small)
            .label(label)
    };
    let percent = viewer
        .percent()
        .map_or_else(String::new, |percent| format!("{percent}%"));
    let bar = row()
        .key("image-viewer-bar")
        .gap(2.0)
        .padding(4.0)
        .center_y()
        .radius(k.radius.lg)
        .fill(k.c.layer_02)
        .border(1.0, k.c.border_base)
        .child(
            icon_button(&icons::MINUS, "Zoom out")
                .on_press(|s: &mut J3| {
                    if let Some(viewer) = s.viewer.as_mut() {
                        viewer.zoom_by(1.0 / STEP, None);
                    }
                })
                .build(k)
                .key("image-viewer-zoom-out"),
        )
        .child(
            el().key("image-viewer-zoom")
                .label(percent.clone())
                .w(48.0)
                .center()
                .child(text(percent).font(k.ty.text_xsmall).color(k.c.text_base)),
        )
        .child(
            icon_button(&icons::PLUS, "Zoom in")
                .on_press(|s: &mut J3| {
                    if let Some(viewer) = s.viewer.as_mut() {
                        viewer.zoom_by(STEP, None);
                    }
                })
                .build(k)
                .key("image-viewer-zoom-in"),
        )
        .child(
            Button::new("Fit")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(|s: &mut J3| {
                    if let Some(viewer) = s.viewer.as_mut() {
                        viewer.camera.reset();
                    }
                })
                .build(k)
                .key("image-viewer-fit"),
        )
        .child(
            Button::new("100%")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(|s: &mut J3| {
                    if let Some(viewer) = s.viewer.as_mut() {
                        viewer.actual_size(None);
                    }
                })
                .build(k)
                .key("image-viewer-actual"),
        )
        .child(
            icon_button(&icons::X, "Close")
                .on_press(|s: &mut J3| s.close_viewer())
                .build(k)
                .key("image-viewer-close"),
        );
    // A bar across the bottom that takes no presses itself, the controls centred in it.
    stage = stage.child(
        row()
            .pin(Some(0.0), None, Some(0.0), Some(16.0))
            .justify_center()
            .child(bar),
    );
    ui.layer.push(stage);
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: [f32; 2] = [1000.0, 800.0];

    #[test]
    fn a_picture_opens_fitted_and_is_never_enlarged_to_fit() {
        let large = [4000.0, 1000.0];
        let camera = Camera::default();
        assert_eq!(Camera::fit(VIEW, large), (1000.0 - 2.0 * MARGIN) / 4000.0);
        let [x, y, w, h] = camera.rect(VIEW, large);
        assert_eq!((x, w), (MARGIN, 1000.0 - 2.0 * MARGIN));
        assert_eq!(y + h / 2.0, 400.0, "centred");
        let small = [100.0, 50.0];
        assert_eq!(camera.rect(VIEW, small), [450.0, 375.0, 100.0, 50.0]);
    }

    #[test]
    fn zooming_holds_the_point_under_the_pointer() {
        let natural = [4000.0, 3000.0];
        let mut camera = Camera::default();
        let at = [300.0, 250.0];
        // The picture's point under the pointer, as a fraction of its width and height.
        let under = |camera: &Camera| {
            let [x, y, w, h] = camera.rect(VIEW, natural);
            [(at[0] - x) / w, (at[1] - y) / h]
        };
        let before = under(&camera);
        camera.zoom_by(2.0, at, VIEW, natural);
        camera.zoom_by(1.5, at, VIEW, natural);
        let after = under(&camera);
        assert!((before[0] - after[0]).abs() < 1e-4 && (before[1] - after[1]).abs() < 1e-4);
        assert!(!camera.fitted());
    }

    #[test]
    fn zoom_stays_between_the_fit_and_the_most() {
        let natural = [4000.0, 3000.0];
        let mut camera = Camera::default();
        let centre = [500.0, 400.0];
        camera.zoom_by(1000.0, centre, VIEW, natural);
        assert_eq!(camera.scale(VIEW, natural), MAX_ZOOM);
        camera.zoom_by(0.0001, centre, VIEW, natural);
        assert!(camera.fitted(), "zooming out to the fit fits again");
        assert_eq!(camera, Camera::default(), "and recentres");
    }

    #[test]
    fn panning_never_shows_space_past_the_pictures_edge() {
        let natural = [2000.0, 400.0];
        let mut camera = Camera::default();
        camera.zoom_to(1.0, [500.0, 400.0], VIEW, natural);
        camera.pan_to([10_000.0, 300.0], VIEW, natural);
        let [x, y, _, h] = camera.rect(VIEW, natural);
        assert_eq!(x, 0.0, "the left edge stops at the view's");
        assert_eq!(y + h / 2.0, 400.0, "a side that fits stays centred");
        camera.pan_to([-10_000.0, 0.0], VIEW, natural);
        let [x, _, w, _] = camera.rect(VIEW, natural);
        assert_eq!(x + w, 1000.0, "the right edge too");
    }

    /// The thread scene with a 240 × 72 point picture open in the viewer, decoded and drawn.
    fn opened() -> crate::app::testing::TestApp {
        use crate::{app::fixtures::Scene, images::ImageLoad};
        let mut app = crate::app::testing::TestApp::with_scene(Scene::Thread);
        app.settle(30);
        let svg = "<svg xmlns='http://www.w3.org/2000/svg' width='240' height='72'/>";
        let ImageLoad::Loaded(image) = app.rt.state.images.decode(svg.into()) else {
            panic!("an SVG decodes");
        };
        for _ in 0..500 {
            if image.size().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let density = app.rt.state.images.density(&image);
        app.rt
            .update(move |s, _| s.open_image(image, density, "Chart"));
        app.settle(30);
        app
    }

    fn picture(app: &crate::app::testing::TestApp) -> [f32; 4] {
        app.rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("image-viewer-image"))
            .map(|e| e.target)
            .expect("the picture draws")
    }

    #[test]
    fn the_viewer_zooms_with_the_wheel_keys_and_buttons_and_closes() {
        let mut app = opened();
        let fitted = picture(&app);
        assert_eq!((fitted[2], fitted[3]), (240.0, 72.0), "its own size");

        // The wheel zooms in; a picture still smaller than the window stays centred.
        let (x, y) = (fitted[0] + 10.0, fitted[1] + 10.0);
        app.rt
            .wheel_event(rune::WheelEvent::new(0.0, 100.0).at(x, y));
        app.frame();
        let zoomed = picture(&app);
        assert!(zoomed[2] > 240.0, "larger: {zoomed:?}");
        assert_eq!(
            zoomed[0] + zoomed[2] / 2.0,
            fitted[0] + fitted[2] / 2.0,
            "centred"
        );

        // Keys reach the viewer, not the composer under it.
        let key = |app: &mut crate::app::testing::TestApp, key: &str| {
            app.key(KeyEvent {
                key: Key::Text(key.to_owned()),
                mods: Mods::default(),
            })
        };
        key(&mut app, "0");
        assert_eq!(picture(&app), fitted, "0 fits");
        key(&mut app, "+");
        assert_eq!(picture(&app)[2], 240.0 * STEP);
        let composer = app.rt.state.main_composer_key().expect("a composer");
        assert_eq!(app.rt.state.composers[&composer].edit.text, "");

        // The bar's buttons.
        let press = |app: &mut crate::app::testing::TestApp, key: &str| {
            let (x, y) = app.rt.find(key).unwrap_or_else(|| panic!("{key} draws"));
            app.rt.pointer_down(x, y);
            app.rt.pointer_up(x, y);
            app.frame();
        };
        press(&mut app, "image-viewer-zoom-in");
        assert_eq!(picture(&app)[2], 240.0 * STEP * STEP);
        press(&mut app, "image-viewer-fit");
        assert_eq!(picture(&app), fitted);

        // A click on the picture keeps it open; one beside it closes.
        app.rt.pointer_down(fitted[0] + 5.0, fitted[1] + 5.0);
        app.rt.pointer_up(fitted[0] + 5.0, fitted[1] + 5.0);
        app.frame();
        assert!(app.rt.state.viewer.is_some());
        app.rt.pointer_down(20.0, 20.0);
        app.rt.pointer_up(20.0, 20.0);
        app.frame();
        assert!(app.rt.state.viewer.is_none());
    }

    #[test]
    fn escape_closes_the_viewer() {
        let mut app = opened();
        app.key(KeyEvent {
            key: Key::Escape,
            mods: Mods::default(),
        });
        assert!(app.rt.state.viewer.is_none());
    }
}
