//! Session GPU work is isolated from the window/input event thread.
use crate::ui::{self, Canvas, Config, Quality, WindowMode};
use crate::window::{Draw, Presenter, Waker};
use crate::{Overlay, RETRY_INTERVAL, clip, placement};
use std::error::Error;
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use topvnc::PerformanceMode;
use topvnc::presentation::{FrameMailbox, FramePacing, MotionPresentation};
use winit::window::Window;

/// Only display settings cross the thread boundary; credentials stay on the UI.
#[derive(Clone, PartialEq)]
pub struct RenderOptions {
    pub window_mode: WindowMode,
    pub quality: Quality,
    pub motion: MotionPresentation,
    pub performance: PerformanceMode,
    pub fps: usize,
    pub progressive: bool,
    pub relative_mouse: bool,
    pub ui_scale: f32,
    pub debug: bool,
    pub open: bool,
    pub locked: bool,
    pub occluded: bool,
    pub serve_scale: Option<u8>,
    pub viewport: (u32, u32),
    pub scale: f64,
}

impl RenderOptions {
    /// Call on the window event thread, where native geometry is available.
    pub fn new(config: &Config, window: &Window) -> Self {
        let size = window.inner_size();
        Self {
            window_mode: config.window_mode,
            quality: config.quality,
            motion: config.motion,
            performance: config.performance,
            fps: config.fps,
            progressive: config.progressive,
            relative_mouse: config.relative_mouse,
            ui_scale: config.ui_scale,
            debug: config.debug,
            open: false,
            locked: false,
            occluded: false,
            serve_scale: None,
            viewport: (size.width, size.height),
            scale: window.scale_factor(),
        }
    }

    fn config(&self) -> Config {
        Config {
            window_mode: self.window_mode,
            quality: self.quality,
            motion: self.motion,
            performance: self.performance,
            fps: self.fps,
            progressive: self.progressive,
            relative_mouse: self.relative_mouse,
            ui_scale: self.ui_scale,
            debug: self.debug,
            ..Config::default()
        }
    }

    fn interval(&self) -> Duration {
        if self.performance == PerformanceMode::Ultra || self.fps == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(1.0 / self.fps as f64)
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RenderSnapshot {
    pub frames: u64,
    pub pacing: Option<(f64, Duration)>,
    pub age_mean: Duration,
    pub mode: Option<wgpu::PresentMode>,
}

struct Control {
    options: RenderOptions,
    revision: u64,
    stopped: bool,
}

struct Shared {
    control: Mutex<Control>,
    changed: Condvar,
    stats: Mutex<RenderSnapshot>,
}

/// Wake generation and condition variable share a lock, preventing missed wakes
/// between checking the mailbox and entering an idle wait.
#[derive(Clone)]
pub struct RenderSignal(Arc<Shared>);

impl RenderSignal {
    pub fn wake(&self) {
        if let Ok(_state) = self.0.control.lock() {
            self.0.changed.notify_one();
        }
    }
}

pub struct RenderWorker {
    signal: RenderSignal,
    worker: Option<JoinHandle<()>>,
}

impl RenderWorker {
    pub fn start(
        presenter: Presenter,
        frames: Arc<FrameMailbox>,
        options: RenderOptions,
        initial: (usize, usize),
        errors: std::sync::mpsc::Sender<io::Result<()>>,
        ui_waker: Waker,
    ) -> io::Result<Self> {
        let signal = RenderSignal(Arc::new(Shared {
            control: Mutex::new(Control {
                options,
                revision: 1,
                stopped: false,
            }),
            changed: Condvar::new(),
            stats: Mutex::new(RenderSnapshot::default()),
        }));
        let shared = Arc::clone(&signal.0);
        let worker = thread::Builder::new()
            .name("topvnc-present".into())
            .spawn(move || {
                if let Err(error) = render_loop(presenter, frames, initial, &shared) {
                    let _ = errors.send(Err(io::Error::other(error.to_string())));
                    ui_waker.wake();
                }
            })?;
        Ok(Self {
            signal,
            worker: Some(worker),
        })
    }

    pub fn signal(&self) -> RenderSignal {
        self.signal.clone()
    }

    pub fn update(&self, options: RenderOptions, force: bool) -> io::Result<()> {
        let mut state = self
            .signal
            .0
            .control
            .lock()
            .map_err(|_| io::Error::other("render control lock poisoned"))?;
        if force || state.options != options {
            state.options = options;
            state.revision = state.revision.wrapping_add(1);
            self.signal.0.changed.notify_one();
        }
        Ok(())
    }

    pub fn snapshot(&self) -> io::Result<RenderSnapshot> {
        self.signal
            .0
            .stats
            .lock()
            .map(|stats| stats.clone())
            .map_err(|_| io::Error::other("render stats lock poisoned"))
    }
}

impl Drop for RenderWorker {
    fn drop(&mut self) {
        if let Ok(mut state) = self.signal.0.control.lock() {
            state.stopped = true;
            self.signal.0.changed.notify_one();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn render_loop(
    mut presenter: Presenter,
    frames: Arc<FrameMailbox>,
    mut remote: (usize, usize),
    shared: &Shared,
) -> Result<(), Box<dyn Error>> {
    let mut image = presenter.layer(remote.0 as u32, remote.1 as u32);
    let mut overlay: Option<Overlay> = None;
    let mut revision = 0;
    let mut redraw = true;
    let mut last_present = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    let mut retry_at = Instant::now();
    let mut motion = None;
    let mut pacing = FramePacing::default();
    let mut pending_arrival = None;
    let mut age_total = Duration::ZERO;
    let mut presented = 0u64;
    loop {
        let mut state = shared
            .control
            .lock()
            .map_err(|_| "render control lock poisoned")?;
        loop {
            if state.stopped {
                return Ok(());
            }
            if state.revision != revision {
                redraw = true;
            }
            let interval = state.options.interval();
            if !state.options.occluded
                && state.options.viewport.0 > 0
                && state.options.viewport.1 > 0
                && (redraw || frames.pending())
            {
                let due = (last_present + interval).max(retry_at);
                let now = Instant::now();
                if now >= due {
                    break;
                }
                state = shared
                    .changed
                    .wait_timeout(state, due - now)
                    .map_err(|_| "render wait lock poisoned")?
                    .0;
            } else {
                if state.options.occluded {
                    pacing.reset();
                }
                state = shared
                    .changed
                    .wait(state)
                    .map_err(|_| "render wait lock poisoned")?;
            }
        }
        let options = state.options.clone();
        revision = state.revision;
        drop(state);
        if motion != Some(options.motion) {
            presenter.set_motion(options.motion);
            motion = Some(options.motion);
            pacing.reset();
        }
        // Acquisition can wait for vsync. Select the newest CPU frame only
        // after that wait, so a busy display does not age a selected snapshot.
        let Some(surface_frame) = presenter.acquire_sized(options.viewport)? else {
            redraw = true;
            retry_at = Instant::now() + RETRY_INTERVAL;
            continue;
        };
        // This worker alone writes the framebuffer texture. The lease holds
        // immutable pixels, but no mutex, while wgpu copies its upload data.
        if let Some(frame) = frames.take()? {
            let pixels = frame.framebuffer();
            let dimensions = (pixels.width(), pixels.height());
            if remote != dimensions {
                remote = dimensions;
                image = presenter.layer(remote.0 as u32, remote.1 as u32);
            }
            let area = frame.damage();
            image.upload(
                pixels.pixels(),
                pixels.width(),
                usize::from(area.x),
                usize::from(area.y),
                usize::from(area.width),
                usize::from(area.height),
            );
            pending_arrival = Some(frame.completed());
        }
        let scale = options.scale;
        let logical = (
            (f64::from(options.viewport.0) / scale).round().max(1.0) as usize,
            (f64::from(options.viewport.1) / scale).round().max(1.0) as usize,
        );
        if overlay
            .as_ref()
            .is_some_and(|overlay| overlay.size != logical)
        {
            overlay = None;
        }
        let overlay = overlay.get_or_insert_with(|| Overlay {
            pixels: vec![0; logical.0 * logical.1],
            size: logical,
            layer: presenter.layer(logical.0 as u32, logical.1 as u32),
        });
        let area = if options.open {
            Some(ui::SETTINGS_PANEL)
        } else if options.locked {
            None
        } else {
            Some(ui::open_settings_box(options.ui_scale))
        }
        .and_then(|area| clip(area, logical));
        // The overlay is small; do not upload it on every incoming game frame.
        if redraw && let Some(area) = area {
            let config = options.config();
            ui::overlay(
                &mut Canvas::new(&mut overlay.pixels, logical.0, logical.1),
                &config,
                options.open,
                options.serve_scale,
            );
            overlay
                .layer
                .upload(&overlay.pixels, logical.0, area.x, area.y, area.w, area.h);
        }
        let physical = |x: usize, y: usize, width: usize, height: usize| {
            [
                x as f64 * scale,
                y as f64 * scale,
                width as f64 * scale,
                height as f64 * scale,
            ]
        };
        let mut draws = Vec::with_capacity(2);
        if let Some((target, source)) = placement(options.window_mode, remote, logical) {
            draws.push(Draw {
                layer: &image,
                target: physical(target.x0, target.y0, target.width(), target.height()),
                source: [
                    source.x0 as f64,
                    source.y0 as f64,
                    source.width() as f64,
                    source.height() as f64,
                ],
                smooth: options.quality == Quality::Smooth,
            });
        }
        if let Some(area) = area {
            draws.push(Draw {
                layer: &overlay.layer,
                target: physical(area.x, area.y, area.w, area.h),
                source: [area.x as f64, area.y as f64, area.w as f64, area.h as f64],
                smooth: scale.fract() != 0.0,
            });
        }
        presenter.draw_acquired(&draws, surface_frame)?;
        last_present = Instant::now();
        if let Some(arrived) = pending_arrival.take() {
            pacing.record(last_present);
            presented += 1;
            age_total += last_present.saturating_duration_since(arrived);
            *shared
                .stats
                .lock()
                .map_err(|_| "render stats lock poisoned")? = RenderSnapshot {
                frames: presented,
                pacing: pacing.summary(),
                age_mean: Duration::from_secs_f64(age_total.as_secs_f64() / presented as f64),
                mode: Some(presenter.present_mode()),
            };
        }
        redraw = false;
    }
}
