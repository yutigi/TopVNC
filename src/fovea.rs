//! Foveated Tight for first-person games (spec 008). JPEG quality falls by
//! zone around the framebuffer center, where the crosshair is, and the
//! center is encoded and sent first. A ladder of rungs follows the link's
//! measured delivery rate; the most compact rungs send the periphery only
//! with every second update.

use crate::{SERVER_TILE_SIZE, ServerRect, tight};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Half-width and half-height of the fovea and mid boxes around the
/// framebuffer center, as fractions of its height.
const FOVEA_BOX: (f32, f32) = (0.30, 0.18);
const MID_BOX: (f32, f32) = (0.60, 0.36);
/// Most rows per rectangle in the fovea, mid zone, and periphery. Short
/// fovea rectangles spread the center over more encoder threads, so it is
/// ready first; every rectangle also costs about 640 bytes of JPEG tables.
const ZONE_ROWS: [usize; 3] = [64, 128, 128];

/// A rectangle as (x, y, width, height) in framebuffer pixels.
pub(crate) type Rect = (usize, usize, usize, usize);

/// The zones around the framebuffer center, nearest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Zone {
    Fovea,
    Mid,
    Periphery,
}

/// The zone of `tile` in a framebuffer of `frame` (width, height) pixels:
/// the box its center falls in.
pub(crate) fn zone((x, y, width, height): Rect, frame: (usize, usize)) -> Zone {
    let dx = (x * 2 + width).abs_diff(frame.0) as f32 / 2.0;
    let dy = (y * 2 + height).abs_diff(frame.1) as f32 / 2.0;
    let unit = frame.1 as f32;
    let inside =
        |(half_width, half_height): (f32, f32)| dx < half_width * unit && dy < half_height * unit;
    if inside(FOVEA_BOX) {
        Zone::Fovea
    } else if inside(MID_BOX) {
        Zone::Mid
    } else {
        Zone::Periphery
    }
}

/// Split changed rectangles at the tile grid and merge the pieces within
/// each zone: along a tile row, then with the run directly above, up to the
/// zone's row limit and Tight's size limit. Every pixel of `rects` is
/// covered once. Returns the rectangles with their zones, fovea first and,
/// within a zone, nearest the framebuffer center first.
pub(crate) fn foveated_rects(rects: &[ServerRect], frame: (usize, usize)) -> Vec<(Rect, Zone)> {
    let mut pieces = Vec::new();
    for rect in rects {
        let (x0, y0) = (usize::from(rect.x), usize::from(rect.y));
        let (x1, y1) = (x0 + usize::from(rect.width), y0 + usize::from(rect.height));
        for tile_y in (y0 / SERVER_TILE_SIZE)..y1.div_ceil(SERVER_TILE_SIZE) {
            for tile_x in (x0 / SERVER_TILE_SIZE)..x1.div_ceil(SERVER_TILE_SIZE) {
                let (tx, ty) = (tile_x * SERVER_TILE_SIZE, tile_y * SERVER_TILE_SIZE);
                let tile = (
                    tx,
                    ty,
                    SERVER_TILE_SIZE.min(frame.0.saturating_sub(tx)),
                    SERVER_TILE_SIZE.min(frame.1.saturating_sub(ty)),
                );
                let (px0, py0) = (tx.max(x0), ty.max(y0));
                let (px1, py1) = (
                    (tx + SERVER_TILE_SIZE).min(x1),
                    (ty + SERVER_TILE_SIZE).min(y1),
                );
                if px0 < px1 && py0 < py1 {
                    pieces.push(((px0, py0, px1 - px0, py1 - py0), zone(tile, frame)));
                }
            }
        }
    }
    pieces.sort_by_key(|&((x, y, ..), _)| (y, x));
    let mut runs: Vec<(Rect, Zone)> = Vec::new();
    for (piece, piece_zone) in pieces {
        if let Some((last, last_zone)) = runs.last_mut()
            && *last_zone == piece_zone
            && last.1 == piece.1
            && last.3 == piece.3
            && last.0 + last.2 == piece.0
            && last.2 + piece.2 <= tight::MAX_RECT_WIDTH
        {
            last.2 += piece.2;
        } else {
            runs.push((piece, piece_zone));
        }
    }
    // Join runs with the run directly above that has the same columns and
    // zone.
    let mut merged: Vec<(Rect, Zone)> = Vec::with_capacity(runs.len());
    let mut open = HashMap::new();
    for ((x, y, width, height), run_zone) in runs {
        let limit = ZONE_ROWS[run_zone as usize].min(tight::MAX_RECT_HEIGHT);
        if let Some(index) = open.remove(&(x, width, y, run_zone)) {
            let (above, _): &mut (Rect, Zone) = &mut merged[index];
            if above.3 + height <= limit {
                above.3 += height;
                open.insert((x, width, y + height, run_zone), index);
                continue;
            }
        }
        open.insert((x, width, y + height, run_zone), merged.len());
        merged.push(((x, y, width, height), run_zone));
    }
    // Squared distance between the centers, doubled to stay in integers.
    let distance = |&((x, y, width, height), _): &(Rect, Zone)| {
        let dx = (x * 2 + width).abs_diff(frame.0);
        let dy = (y * 2 + height).abs_diff(frame.1);
        dx * dx + dy * dy
    };
    merged.sort_by_key(|rect| (rect.1, distance(rect)));
    merged
}

/// A step of the quality ladder: the quality levels of the fovea, mid zone,
/// and periphery for a client at [`RUNG_BASE_LEVEL`], and the periphery
/// interval: periphery tiles go with every Nth update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rung {
    levels: [u8; 3],
    periphery_interval: u32,
}

/// Sharpest first: Sharp, Balanced, Wi-Fi, then Wi-Fi with the periphery
/// every second update, then a more compact center as well.
const RUNGS: [Rung; 5] = [
    Rung {
        levels: [8, 5, 2],
        periphery_interval: 1,
    },
    Rung {
        levels: [6, 4, 1],
        periphery_interval: 1,
    },
    Rung {
        levels: [5, 3, 0],
        periphery_interval: 1,
    },
    Rung {
        levels: [5, 3, 0],
        periphery_interval: 2,
    },
    Rung {
        levels: [4, 1, 0],
        periphery_interval: 2,
    },
];
/// Every session starts at Balanced.
const START_RUNG: usize = 1;
/// The client quality level [`RUNGS`] are written for. Other clients get
/// the table shifted by their difference from it.
const RUNG_BASE_LEVEL: i32 = 6;
/// Bytes of each rung's update relative to the next more compact rung's,
/// measured on five 1080p FPS screenshots; predicts whether a climb fits.
const CLIMB_GROWTH: [f64; 4] = [1.44, 1.31, 1.18, 1.3];
/// Step down once updates outgrow this fraction of the link's capacity.
/// The delivery rate includes the viewer's decode time, so on a link that
/// does not bank idle time this is about 80% of what it carries.
const STEP_DOWN_LOAD: f64 = 0.9;
/// Climb once the sharper rung's predicted updates stay under this
/// fraction. The gap to [`STEP_DOWN_LOAD`] keeps a rung from flapping, and
/// a climb that overflows anyway backs off.
const CLIMB_LOAD: f64 = 0.8;
/// Updates at a rung before its smoothed size counts.
const MIN_RUNG_UPDATES: u32 = 8;
/// The wait before the next climb: after a climb, and after a step down.
const CLIMB_HOLD: Duration = Duration::from_millis(500);
const MIN_CLIMB_BACKOFF: Duration = Duration::from_secs(1);
/// A step down this soon after a climb shows the climb overflowed, and the
/// wait after the next step down doubles, up to [`MAX_CLIMB_BACKOFF`].
const CLIMB_TRIAL: Duration = Duration::from_secs(2);
const MAX_CLIMB_BACKOFF: Duration = Duration::from_secs(16);
/// The shortest host frame interval budgets are computed for.
const MIN_FRAME_INTERVAL: f64 = 1.0 / 480.0;
/// A longer gap between changed host frames is idle time, not a frame
/// interval.
const IDLE_FRAME_GAP: f64 = 0.1;

/// A client session's foveation state: its rung, the adaptation that moves
/// it, the host's frame rate, and the periphery schedule.
#[derive(Debug)]
pub(crate) struct State {
    rung: usize,
    /// Bytes per update at this rung, smoothed, and the updates counted.
    bytes: f64,
    updates: u32,
    /// The rung climbs no earlier than this.
    climb_after: Option<Instant>,
    /// When the rung last climbed, until a step down examines it.
    climbed_at: Option<Instant>,
    /// The wait after a step down before the next climb.
    backoff: Duration,
    /// Seconds between host frames that changed pixels, smoothed, and the
    /// last frame count seen with when it was seen.
    frame_interval: f64,
    last_frames: Option<(u64, Instant)>,
    /// Updates with fovea or mid tiles since periphery tiles last went out,
    /// and when that was.
    updates_since_periphery: u32,
    periphery_sent_at: Option<Instant>,
    /// When held-back periphery tiles are due, while there are any.
    periphery_due: Option<Instant>,
}

impl State {
    pub(crate) fn new() -> Self {
        Self {
            rung: START_RUNG,
            bytes: 0.0,
            updates: 0,
            climb_after: None,
            climbed_at: None,
            backoff: MIN_CLIMB_BACKOFF,
            frame_interval: 0.0,
            last_frames: None,
            updates_since_periphery: 0,
            periphery_sent_at: None,
            periphery_due: None,
        }
    }

    /// Quality levels of the fovea, mid zone, and periphery, indexed by
    /// [`Zone`], for a client that asked for `level`: the rung's levels
    /// shifted by the client's difference from [`RUNG_BASE_LEVEL`].
    pub(crate) fn levels(&self, level: u8) -> [u8; 3] {
        let shift = i32::from(level) - RUNG_BASE_LEVEL;
        RUNGS[self.rung]
            .levels
            .map(|zone| (i32::from(zone) + shift).clamp(0, 9) as u8)
    }

    /// Track the host's frame rate from its count of frames that changed
    /// pixels, `frames`, seen at `now`. A sample spanning several frames
    /// weighs as much as that many one-frame samples, so the smoothed
    /// interval is the mean however often the count is seen.
    pub(crate) fn observe_frames(&mut self, frames: u64, now: Instant) {
        match self.last_frames {
            Some((last, _)) if frames == last => return,
            Some((last, at)) if frames > last => {
                let count = frames - last;
                let sample = now.saturating_duration_since(at).as_secs_f64() / count as f64;
                if sample <= IDLE_FRAME_GAP {
                    let weight = if self.frame_interval == 0.0 {
                        1.0
                    } else {
                        1.0 - 0.9f64.powi(count.min(64) as i32)
                    };
                    self.frame_interval += weight * (sample - self.frame_interval);
                }
            }
            // The first count, or the host restarted it.
            _ => {}
        }
        self.last_frames = Some((frames, now));
    }

    /// The bytes the link carries in one host frame interval when it
    /// delivers `rate` bytes per second.
    fn capacity(&self, rate: Option<f64>) -> Option<f64> {
        let rate = rate?;
        (self.frame_interval > 0.0).then(|| rate * self.frame_interval.max(MIN_FRAME_INTERVAL))
    }

    /// After a continuous update of `bytes` sent at `now`, move to a more
    /// compact rung when updates outgrow the link, which delivers `rate`
    /// bytes per second, or to a sharper one when it clearly fits.
    pub(crate) fn sent(&mut self, bytes: usize, rate: Option<f64>, now: Instant) {
        let bytes = bytes as f64;
        self.bytes = if self.updates == 0 {
            bytes
        } else {
            0.8 * self.bytes + 0.2 * bytes
        };
        self.updates += 1;
        let Some(capacity) = self.capacity(rate) else {
            return;
        };
        if self.updates < MIN_RUNG_UPDATES {
            return;
        }
        if self.bytes > STEP_DOWN_LOAD * capacity && self.rung + 1 < RUNGS.len() {
            let overflowed = self
                .climbed_at
                .take()
                .is_some_and(|at| now.saturating_duration_since(at) < CLIMB_TRIAL);
            self.backoff = if overflowed {
                (self.backoff * 2).min(MAX_CLIMB_BACKOFF)
            } else {
                MIN_CLIMB_BACKOFF
            };
            self.change(self.rung + 1, now + self.backoff);
        } else if self.rung > 0
            && self.climb_after.is_none_or(|after| now >= after)
            && self.bytes * CLIMB_GROWTH[self.rung - 1] < CLIMB_LOAD * capacity
        {
            self.climbed_at = Some(now);
            self.change(self.rung - 1, now + CLIMB_HOLD);
        }
    }

    fn change(&mut self, rung: usize, climb_after: Instant) {
        self.rung = rung;
        self.bytes = 0.0;
        self.updates = 0;
        self.climb_after = Some(climb_after);
    }

    /// Whether a continuous update's periphery tiles go out now, at `now`.
    /// `has_center` and `has_periphery` say whether it has fovea or mid
    /// tiles and periphery tiles. With interval N, periphery tiles go with
    /// every Nth update that has center tiles: counting updates, not host
    /// frames, which a congested update spans several of. Without center
    /// tiles they go once N host frame intervals have passed since the
    /// periphery last went out.
    pub(crate) fn send_periphery(
        &mut self,
        has_center: bool,
        has_periphery: bool,
        now: Instant,
    ) -> bool {
        let interval = RUNGS[self.rung].periphery_interval;
        let wait = Duration::from_secs_f64(
            self.frame_interval.max(MIN_FRAME_INTERVAL) * f64::from(interval),
        );
        let send = !has_periphery
            || interval <= 1
            || if has_center {
                self.updates_since_periphery + 1 >= interval
            } else {
                self.periphery_sent_at
                    .is_none_or(|at| now.saturating_duration_since(at) >= wait)
            };
        if has_periphery && send {
            self.updates_since_periphery = 0;
            self.periphery_sent_at = Some(now);
        } else {
            self.updates_since_periphery = self
                .updates_since_periphery
                .saturating_add(u32::from(has_center));
        }
        self.periphery_due = (!send).then(|| self.periphery_sent_at.map_or(now, |at| at + wait));
        send
    }

    /// When held-back periphery tiles are due, while there are any.
    pub(crate) fn periphery_due(&self) -> Option<Instant> {
        self.periphery_due
    }

    /// Record that an update went out without the periphery schedule, so
    /// nothing is held back.
    pub(crate) fn periphery_released(&mut self) {
        self.periphery_due = None;
    }

    #[cfg(test)]
    pub(crate) fn at_rung(rung: usize) -> Self {
        Self {
            rung,
            ..Self::new()
        }
    }

    #[cfg(test)]
    pub(crate) fn rung(&self) -> usize {
        self.rung
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiles(width: usize, height: usize) -> Vec<ServerRect> {
        let mut changed = Vec::new();
        for y in (0..height).step_by(SERVER_TILE_SIZE) {
            for x in (0..width).step_by(SERVER_TILE_SIZE) {
                changed.push(ServerRect {
                    x: x as u16,
                    y: y as u16,
                    width: SERVER_TILE_SIZE.min(width - x) as u16,
                    height: SERVER_TILE_SIZE.min(height - y) as u16,
                    tile_index: None,
                    revision: 0,
                });
            }
        }
        changed
    }

    #[test]
    fn zones_snap_to_tiles_around_the_center() {
        let frame = (1920, 1080);
        let extent = |wanted: Zone| {
            let mut bounds = (usize::MAX, usize::MAX, 0, 0);
            for y in (0..1080).step_by(64) {
                for x in (0..1920).step_by(64) {
                    let tile = (x, y, 64.min(1920 - x), 64.min(1080 - y));
                    if zone(tile, frame) <= wanted {
                        bounds = (
                            bounds.0.min(x),
                            bounds.1.min(y),
                            bounds.2.max(x + tile.2),
                            bounds.3.max(y + tile.3),
                        );
                    }
                }
            }
            bounds
        };
        assert_eq!(extent(Zone::Fovea), (640, 320, 1280, 704));
        assert_eq!(extent(Zone::Mid), (320, 128, 1600, 960));
        assert_eq!(zone((0, 0, 64, 64), frame), Zone::Periphery);
        assert_eq!(zone((1856, 1024, 64, 56), frame), Zone::Periphery);
    }

    #[test]
    fn foveated_rects_cover_changes_once_and_send_the_center_first() {
        for (width, height) in [(1920, 1080), (1512, 982), (100, 70), (3024, 1964), (64, 64)] {
            let mut changed = tiles(width, height);
            // A request rectangle that is not tile-aligned overlaps them.
            changed.push(ServerRect {
                x: 10,
                y: 20,
                width: (width - 10).min(150) as u16,
                height: (height - 20).min(90) as u16,
                tile_index: None,
                revision: 0,
            });
            let rects = foveated_rects(&changed, (width, height));
            let mut covered = vec![0u8; width * height];
            for &((x, y, w, h), rect_zone) in &rects {
                assert!(w <= tight::MAX_RECT_WIDTH && h <= ZONE_ROWS[rect_zone as usize]);
                for row in y..y + h {
                    for count in &mut covered[row * width + x..row * width + x + w] {
                        *count += 1;
                    }
                }
            }
            // The overlap is sent twice, as the changes were reported.
            let expected = |index: usize| {
                let (x, y) = (index % width, index / width);
                1 + u8::from((10..160).contains(&x) && (20..110).contains(&y))
            };
            assert!(
                covered
                    .iter()
                    .enumerate()
                    .all(|(index, count)| *count == expected(index)),
                "{width}x{height}"
            );
            assert!(rects.windows(2).all(|pair| pair[0].1 <= pair[1].1));
            if width >= 1000 {
                // The first rectangle is in the fovea, next to the center.
                let ((x, y, w, h), first_zone) = rects[0];
                assert_eq!(first_zone, Zone::Fovea);
                assert!(x <= width / 2 && width / 2 <= x + w);
                assert!(y.abs_diff(height / 2) <= 64 && (y + h).abs_diff(height / 2) <= 128);
            }
        }
        // A full 1080p update is 32 rectangles: the fovea in six 64-row
        // bands, the rest in bands of up to 128 rows.
        let rects = foveated_rects(&tiles(1920, 1080), (1920, 1080));
        assert_eq!(rects.len(), 32);
        assert_eq!(
            rects
                .iter()
                .filter(|(_, zone)| *zone == Zone::Fovea)
                .count(),
            6
        );
        assert_eq!(rects[0].0, (640, 512, 640, 64));
    }

    #[test]
    fn zone_levels_follow_the_rung_and_the_clients_level() {
        let balanced = State::new();
        assert_eq!(balanced.levels(6), [6, 4, 1]);
        assert_eq!(balanced.levels(8), [8, 6, 3]);
        assert_eq!(balanced.levels(3), [3, 1, 0]);
        assert_eq!(balanced.levels(0), [0, 0, 0]);
        assert_eq!(balanced.levels(9), [9, 7, 4]);
        assert_eq!(State::at_rung(0).levels(6), [8, 5, 2]);
        assert_eq!(State::at_rung(0).levels(9), [9, 8, 5]);
        assert_eq!(State::at_rung(4).levels(6), [4, 1, 0]);
    }

    #[test]
    fn frame_interval_ignores_idle_gaps_and_restarted_counts() {
        let start = Instant::now();
        let at = |ms: f64| start + Duration::from_secs_f64(ms / 1000.0);
        let mut state = State::new();
        state.observe_frames(10, at(0.0));
        assert_eq!(state.frame_interval, 0.0);
        // Two frames in 20 ms.
        state.observe_frames(12, at(20.0));
        assert!((state.frame_interval - 0.010).abs() < 1e-9);
        // Three frames in 15 ms weigh as much as three one-frame samples.
        let mut weighted = State::new();
        weighted.observe_frames(0, at(0.0));
        weighted.observe_frames(1, at(10.0));
        weighted.observe_frames(4, at(25.0));
        let expected = 0.010 + (1.0 - 0.9f64.powi(3)) * (0.005 - 0.010);
        assert!((weighted.frame_interval - expected).abs() < 1e-9);
        // Seeing the same count again changes nothing.
        state.observe_frames(12, at(25.0));
        state.observe_frames(13, at(30.0));
        assert!((state.frame_interval - 0.010).abs() < 1e-9);
        // A pause is not a frame interval.
        state.observe_frames(14, at(2030.0));
        assert!((state.frame_interval - 0.010).abs() < 1e-9);
        state.observe_frames(0, at(2040.0));
        state.observe_frames(1, at(2045.0));
        assert!((state.frame_interval - (0.9 * 0.010 + 0.1 * 0.005)).abs() < 1e-9);
    }

    /// A link that delivers `rate` bytes per second to a host at 120 fps:
    /// sends `count` updates sized for each rung by `sizes`, one per frame,
    /// starting at `start`, and returns the rungs used.
    fn simulate(
        state: &mut State,
        start: Instant,
        count: u32,
        rate: f64,
        sizes: [f64; 5],
    ) -> Vec<usize> {
        let interval = Duration::from_secs_f64(1.0 / 120.0);
        let mut rungs = Vec::new();
        for frame in 0..count {
            let now = start + interval * frame;
            state.observe_frames(u64::from(frame), now);
            state.sent(sizes[state.rung] as usize, Some(rate), now);
            rungs.push(state.rung);
        }
        rungs
    }

    /// Update sizes of the five rungs, in bytes, from Balanced at `balanced`
    /// and the measured ratios between neighbors.
    fn sizes(balanced: f64) -> [f64; 5] {
        let mut sizes = [0.0; 5];
        sizes[1] = balanced;
        sizes[0] = balanced * CLIMB_GROWTH[0];
        for rung in 2..5 {
            sizes[rung] = sizes[rung - 1] / CLIMB_GROWTH[rung - 1];
        }
        sizes
    }

    #[test]
    fn rungs_climb_on_a_fast_link_and_stay() {
        let start = Instant::now();
        let mut state = State::new();
        // 1 Gbit/s carries 1 MB per frame at 120 fps; Sharp needs 280 KB.
        let rungs = simulate(&mut state, start, 600, 125e6, sizes(194e3));
        assert_eq!(rungs[0], 1);
        let climbed = rungs.iter().position(|rung| *rung == 0).unwrap();
        assert!(climbed < 12, "climbed after {climbed} updates");
        assert!(rungs[climbed..].iter().all(|rung| *rung == 0));
    }

    #[test]
    fn rungs_step_down_to_what_a_slow_link_carries() {
        let start = Instant::now();
        let mut state = State::new();
        // 144 Mbit/s carries 150 KB per frame at 120 fps, 135 KB within the
        // step-down load: rung 3 (126 KB) fits, Wi-Fi (148 KB) does not.
        let rungs = simulate(&mut state, start, 1200, 18e6, sizes(194e3));
        assert_eq!(*rungs.last().unwrap(), 3);
        let settled = rungs.iter().position(|rung| *rung == 3).unwrap();
        assert!(settled < 30, "settled after {settled} updates");
        // It never climbs back into a rung that overflows.
        assert!(rungs[settled..].iter().all(|rung| *rung == 3));
    }

    #[test]
    fn rungs_hold_balanced_near_a_links_limit() {
        let start = Instant::now();
        let mut state = State::new();
        // 300 Mbit/s carries 312 KB per frame. Balanced (194 KB, 62%) fits,
        // and Sharp's predicted 280 KB is above the 80% climb load.
        let rungs = simulate(&mut state, start, 1200, 37.5e6, sizes(194e3));
        assert!(rungs.iter().all(|rung| *rung == 1));
    }

    #[test]
    fn a_climb_that_overflows_waits_longer_each_time() {
        let start = Instant::now();
        let mut state = State::new();
        // 500 KB per frame: Sharp's predicted 280 KB fits, but its real
        // updates are twice that and overflow the link.
        let mut sizes = sizes(194e3);
        sizes[0] *= 2.0;
        let rungs = simulate(&mut state, start, 120 * 40, 60e6, sizes);
        let climbs: Vec<usize> = rungs
            .windows(2)
            .enumerate()
            .filter(|(_, pair)| pair[1] < pair[0])
            .map(|(frame, _)| frame + 1)
            .collect();
        // Waits of 2, 4, 8, and then 16 s between climbs.
        assert!(climbs.len() >= 3 && climbs.len() <= 6, "{climbs:?}");
        let gaps: Vec<usize> = climbs.windows(2).map(|pair| pair[1] - pair[0]).collect();
        assert!(gaps.windows(2).all(|pair| pair[1] > pair[0]), "{gaps:?}");
        assert!(gaps.iter().all(|gap| *gap <= 120 * 17), "{gaps:?}");
        // Most of the time is spent on the rung that fits.
        assert!(rungs.iter().filter(|rung| **rung == 1).count() > rungs.len() * 9 / 10);
    }

    #[test]
    fn rungs_stay_put_without_a_rate_or_a_frame_rate() {
        let start = Instant::now();
        let mut state = State::new();
        for frame in 0..100 {
            state.sent(1_000_000, None, start + Duration::from_millis(frame * 8));
        }
        assert_eq!(state.rung(), START_RUNG);
        // A rate alone is not enough: the frame interval is unknown.
        for frame in 0..100 {
            state.sent(
                1_000_000,
                Some(1e6),
                start + Duration::from_millis(frame * 8),
            );
        }
        assert_eq!(state.rung(), START_RUNG);
    }

    #[test]
    fn periphery_goes_with_every_second_update_or_after_two_frame_intervals() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut state = State::at_rung(3);
        state.observe_frames(0, at(0));
        state.observe_frames(1, at(10));
        // With center tiles: held back, then sent with the second update.
        assert!(!state.send_periphery(true, true, at(10)));
        assert_eq!(state.periphery_due(), Some(at(10)));
        assert!(state.send_periphery(true, true, at(20)));
        assert_eq!(state.periphery_due(), None);
        assert!(!state.send_periphery(true, true, at(30)));
        // Two frame intervals (20 ms) after the last periphery.
        assert_eq!(state.periphery_due(), Some(at(40)));
        // Without center tiles the periphery waits for that time.
        assert!(!state.send_periphery(false, true, at(35)));
        assert!(state.send_periphery(false, true, at(40)));
        // Updates without periphery changes count toward the interval, so
        // a periphery change after a quiet spell goes out at once.
        assert!(state.send_periphery(true, false, at(50)));
        assert!(state.send_periphery(true, true, at(60)));
        assert!(!state.send_periphery(true, true, at(70)));
        state.periphery_released();
        assert_eq!(state.periphery_due(), None);
        // Rungs that send the periphery every update never hold it back.
        let mut balanced = State::new();
        assert!(balanced.send_periphery(true, true, at(0)));
        assert!(balanced.send_periphery(false, true, at(0)));
    }
}
