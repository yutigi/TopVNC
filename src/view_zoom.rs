//! Local zoom and pan of the remote image. It changes only what this viewer
//! shows; the host keeps sending the same framebuffer.
//!
//! At 1x the window draws exactly as `placement` says. Zoomed in, the remote
//! image is scaled by `factor` on top of the mode's own scale (fit or 1:1)
//! and may grow past the letterbox to fill the window. Geometry is in
//! logical window pixels and remote framebuffer pixels, as `f64`, so a deep
//! zoom can show a fraction of one remote pixel.
use crate::ui::WindowMode;

/// The deepest zoom, relative to the window mode's own scale.
pub const MAX_ZOOM: f64 = 800.0;

/// From this many logical pixels per remote pixel up, draw remote pixels as
/// sharp squares instead of blending them.
const PIXELATED_FROM: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewZoom {
    factor: f64,
    /// The remote point at the window's center, in remote pixels; `None`
    /// means the middle of the remote image.
    center: Option<(f64, f64)>,
}

impl Default for ViewZoom {
    fn default() -> Self {
        Self {
            factor: 1.0,
            center: None,
        }
    }
}

/// How remote pixels land in the window: `window = (remote - origin) * scale`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Mapping {
    scale: f64,
    origin: (f64, f64),
}

fn valid(remote: (usize, usize), window: (usize, usize)) -> bool {
    remote.0 > 0 && remote.1 > 0 && window.0 > 0 && window.1 > 0
}

/// Logical pixels per remote pixel at 1x.
fn base_scale(mode: WindowMode, remote: (usize, usize), window: (usize, usize)) -> f64 {
    match mode {
        WindowMode::Native => 1.0,
        WindowMode::Fit | WindowMode::Custom => {
            (window.0 as f64 / remote.0 as f64).min(window.1 as f64 / remote.1 as f64)
        }
    }
}

/// Keep `center` where the image covers the window on this axis, or in the
/// middle where it is smaller than the window.
fn clamp_axis(center: f64, remote: usize, window: usize, scale: f64) -> f64 {
    let remote = remote as f64;
    let visible = window as f64 / scale;
    if visible >= remote {
        remote / 2.0
    } else {
        center.clamp(visible / 2.0, remote - visible / 2.0)
    }
}

impl ViewZoom {
    pub fn factor(&self) -> f64 {
        self.factor
    }

    pub fn is_zoomed(&self) -> bool {
        self.factor > 1.0
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn mapping(
        &self,
        mode: WindowMode,
        remote: (usize, usize),
        window: (usize, usize),
    ) -> Option<Mapping> {
        if !valid(remote, window) {
            return None;
        }
        let scale = base_scale(mode, remote, window) * self.factor;
        let (cx, cy) = self
            .center
            .unwrap_or((remote.0 as f64 / 2.0, remote.1 as f64 / 2.0));
        let cx = clamp_axis(cx, remote.0, window.0, scale);
        let cy = clamp_axis(cy, remote.1, window.1, scale);
        Some(Mapping {
            scale,
            origin: (
                cx - window.0 as f64 / 2.0 / scale,
                cy - window.1 as f64 / 2.0 / scale,
            ),
        })
    }

    /// Store the center as the clamped one, so a pan past an edge does not
    /// have to be undone before the view moves back.
    fn settle(&mut self, mode: WindowMode, remote: (usize, usize), window: (usize, usize)) {
        if self.factor <= 1.0 {
            self.reset();
            return;
        }
        if let Some(mapping) = self.mapping(mode, remote, window) {
            self.center = Some((
                mapping.origin.0 + window.0 as f64 / 2.0 / mapping.scale,
                mapping.origin.1 + window.1 as f64 / 2.0 / mapping.scale,
            ));
        }
    }

    /// Multiply the zoom by `by`, keeping the remote point under the window
    /// point `at` where it is, unless an edge stops the view. Returns whether
    /// the view changed.
    pub fn zoom_at(
        &mut self,
        by: f64,
        at: (f64, f64),
        mode: WindowMode,
        remote: (usize, usize),
        window: (usize, usize),
    ) -> bool {
        let before = *self;
        let Some(mapping) = self.mapping(mode, remote, window) else {
            return false;
        };
        if !by.is_finite() || by <= 0.0 || !at.0.is_finite() || !at.1.is_finite() {
            return false;
        }
        let anchor = (
            mapping.origin.0 + at.0 / mapping.scale,
            mapping.origin.1 + at.1 / mapping.scale,
        );
        self.factor = (self.factor * by).clamp(1.0, MAX_ZOOM);
        let scale = base_scale(mode, remote, window) * self.factor;
        self.center = Some((
            anchor.0 - at.0 / scale + window.0 as f64 / 2.0 / scale,
            anchor.1 - at.1 / scale + window.1 as f64 / 2.0 / scale,
        ));
        self.settle(mode, remote, window);
        *self != before
    }

    /// Move the image by `delta` logical pixels, as a drag does.
    pub fn pan(
        &mut self,
        delta: (f64, f64),
        mode: WindowMode,
        remote: (usize, usize),
        window: (usize, usize),
    ) -> bool {
        let before = *self;
        if !self.is_zoomed() || !delta.0.is_finite() || !delta.1.is_finite() {
            return false;
        }
        let Some(mapping) = self.mapping(mode, remote, window) else {
            return false;
        };
        let (cx, cy) = (
            mapping.origin.0 + window.0 as f64 / 2.0 / mapping.scale,
            mapping.origin.1 + window.1 as f64 / 2.0 / mapping.scale,
        );
        self.center = Some((cx - delta.0 / mapping.scale, cy - delta.1 / mapping.scale));
        self.settle(mode, remote, window);
        *self != before
    }

    /// Logical pixels per remote pixel as shown.
    pub fn pixel_scale(
        &self,
        mode: WindowMode,
        remote: (usize, usize),
        window: (usize, usize),
    ) -> f64 {
        self.mapping(mode, remote, window)
            .map_or(1.0, |mapping| mapping.scale)
    }

    /// Whether remote pixels are large enough to draw as sharp squares.
    pub fn pixelated(
        &self,
        mode: WindowMode,
        remote: (usize, usize),
        window: (usize, usize),
    ) -> bool {
        self.is_zoomed() && self.pixel_scale(mode, remote, window) >= PIXELATED_FROM
    }

    /// Where the zoomed image is drawn, in logical window pixels, and which
    /// part of the remote image, each as x, y, width, height. `None` at 1x,
    /// where `placement` decides.
    pub fn draw(
        &self,
        mode: WindowMode,
        remote: (usize, usize),
        window: (usize, usize),
    ) -> Option<([f64; 4], [f64; 4])> {
        if !self.is_zoomed() {
            return None;
        }
        let Mapping { scale, origin } = self.mapping(mode, remote, window)?;
        let x0 = (-origin.0 * scale).max(0.0);
        let y0 = (-origin.1 * scale).max(0.0);
        let x1 = ((remote.0 as f64 - origin.0) * scale).min(window.0 as f64);
        let y1 = ((remote.1 as f64 - origin.1) * scale).min(window.1 as f64);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some((
            [x0, y0, x1 - x0, y1 - y0],
            [
                origin.0 + x0 / scale,
                origin.1 + y0 / scale,
                (x1 - x0) / scale,
                (y1 - y0) / scale,
            ],
        ))
    }

    /// The remote pixel under the logical window point `at` while zoomed;
    /// `None` at 1x, where the window mode maps the pointer.
    pub fn remote_pointer(
        &self,
        at: (f64, f64),
        mode: WindowMode,
        remote: (usize, usize),
        window: (usize, usize),
    ) -> Option<(u16, u16)> {
        if !self.is_zoomed() || !at.0.is_finite() || !at.1.is_finite() {
            return None;
        }
        let Mapping { scale, origin } = self.mapping(mode, remote, window)?;
        let x = (origin.0 + at.0 / scale)
            .floor()
            .clamp(0.0, (remote.0 - 1) as f64);
        let y = (origin.1 + at.1 / scale)
            .floor()
            .clamp(0.0, (remote.1 - 1) as f64);
        Some((x as u16, y as u16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REMOTE: (usize, usize) = (1920, 1080);
    const WINDOW: (usize, usize) = (960, 540);

    #[test]
    fn one_x_defers_to_the_window_mode() {
        let zoom = ViewZoom::default();
        assert!(!zoom.is_zoomed());
        assert_eq!(zoom.draw(WindowMode::Fit, REMOTE, WINDOW), None);
        assert_eq!(
            zoom.remote_pointer((10.0, 10.0), WindowMode::Fit, REMOTE, WINDOW),
            None
        );
        assert!(!zoom.pixelated(WindowMode::Fit, REMOTE, WINDOW));
    }

    #[test]
    fn zooms_around_the_pointer() {
        let mut zoom = ViewZoom::default();
        // Fit: 0.5 logical pixels per remote pixel, so (300, 100) shows (600, 200).
        assert!(zoom.zoom_at(4.0, (300.0, 100.0), WindowMode::Fit, REMOTE, WINDOW));
        assert_eq!(zoom.factor(), 4.0);
        assert_eq!(
            zoom.remote_pointer((300.0, 100.0), WindowMode::Fit, REMOTE, WINDOW),
            Some((600, 200))
        );
        let (target, source) = zoom.draw(WindowMode::Fit, REMOTE, WINDOW).unwrap();
        assert_eq!(target, [0.0, 0.0, 960.0, 540.0]);
        // Two logical pixels per remote pixel: a quarter of the image.
        assert_eq!(source, [450.0, 150.0, 480.0, 270.0]);
        assert!(zoom.pixelated(WindowMode::Fit, REMOTE, WINDOW));
        // Zooming back out to 1x resets.
        assert!(zoom.zoom_at(0.01, (0.0, 0.0), WindowMode::Fit, REMOTE, WINDOW));
        assert_eq!(zoom, ViewZoom::default());
    }

    #[test]
    fn reaches_800x_and_shows_a_fraction_of_a_pixel() {
        let mut zoom = ViewZoom::default();
        for _ in 0..100 {
            zoom.zoom_at(1.25, (480.0, 270.0), WindowMode::Native, REMOTE, WINDOW);
        }
        assert_eq!(zoom.factor(), MAX_ZOOM);
        let (_, source) = zoom.draw(WindowMode::Native, REMOTE, WINDOW).unwrap();
        assert!((source[2] - 960.0 / 800.0).abs() < 1e-9);
        assert!((source[3] - 540.0 / 800.0).abs() < 1e-9);
        // The pointed-at pixel stayed under the pointer the whole way.
        assert_eq!(
            zoom.remote_pointer((480.0, 270.0), WindowMode::Native, REMOTE, WINDOW),
            Some((960, 540))
        );
        assert!(!zoom.zoom_at(2.0, (0.0, 0.0), WindowMode::Native, REMOTE, WINDOW));
    }

    #[test]
    fn panning_stops_at_the_edges_and_reverses_at_once() {
        let mut zoom = ViewZoom::default();
        zoom.zoom_at(2.0, (480.0, 270.0), WindowMode::Fit, REMOTE, WINDOW);
        assert!(zoom.pan((1e9, 1e9), WindowMode::Fit, REMOTE, WINDOW));
        let (_, source) = zoom.draw(WindowMode::Fit, REMOTE, WINDOW).unwrap();
        assert_eq!(source, [0.0, 0.0, 960.0, 540.0]);
        assert!(!zoom.pan((1.0, 1.0), WindowMode::Fit, REMOTE, WINDOW));
        assert!(zoom.pan((-10.0, 0.0), WindowMode::Fit, REMOTE, WINDOW));
        let (_, source) = zoom.draw(WindowMode::Fit, REMOTE, WINDOW).unwrap();
        assert_eq!(source[0], 10.0);
        zoom.pan((-1e9, -1e9), WindowMode::Fit, REMOTE, WINDOW);
        assert_eq!(
            zoom.remote_pointer((959.9, 539.9), WindowMode::Fit, REMOTE, WINDOW),
            Some((1919, 1079))
        );
    }

    #[test]
    fn a_letterboxed_image_grows_into_the_bars_then_covers_the_window() {
        // A square window: bars above and below the 16:9 image at 1x.
        let window = (900, 900);
        let mut zoom = ViewZoom::default();
        zoom.zoom_at(1.5, (450.0, 450.0), WindowMode::Fit, REMOTE, window);
        let (target, source) = zoom.draw(WindowMode::Fit, REMOTE, window).unwrap();
        // Still shorter than the window: centered, whole height shown.
        assert!((target[1] - (900.0 - 759.375) / 2.0).abs() < 1e-9);
        assert_eq!(target[0], 0.0);
        assert!((source[3] - 1080.0).abs() < 1e-9);
        zoom.zoom_at(2.0, (450.0, 450.0), WindowMode::Fit, REMOTE, window);
        let (target, _) = zoom.draw(WindowMode::Fit, REMOTE, window).unwrap();
        assert_eq!(target, [0.0, 0.0, 900.0, 900.0]);
    }

    #[test]
    fn rejects_unusable_input() {
        let mut zoom = ViewZoom::default();
        assert!(!zoom.zoom_at(2.0, (0.0, 0.0), WindowMode::Fit, (0, 0), WINDOW));
        assert!(!zoom.zoom_at(f64::NAN, (0.0, 0.0), WindowMode::Fit, REMOTE, WINDOW));
        assert!(!zoom.zoom_at(2.0, (f64::INFINITY, 0.0), WindowMode::Fit, REMOTE, WINDOW));
        assert!(!zoom.pan((5.0, 5.0), WindowMode::Fit, REMOTE, WINDOW));
        zoom.zoom_at(2.0, (0.0, 0.0), WindowMode::Fit, REMOTE, WINDOW);
        assert_eq!(zoom.draw(WindowMode::Fit, REMOTE, (0, 10)), None);
    }
}
