//! Bounded CPU handoff between decoding and presentation. No window or GPU APIs.
use crate::{DamageRect, Framebuffer};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MotionPresentation {
    /// Synchronize complete images to the display without a deep frame queue.
    #[default]
    Smooth,
    /// Present immediately; may tear on supported surfaces.
    Immediate,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MailboxStats {
    pub published: u64,
    pub replaced: u64,
    pub consumed: u64,
    pub copied_pixels: u64,
}

struct Slot {
    framebuffer: Framebuffer,
    stale: Option<DamageRect>,
}

struct ReadyFrame {
    slot: Slot,
    damage: DamageRect,
    completed: Instant,
}

struct State {
    dimensions: (u16, u16),
    ready: Option<ReadyFrame>,
    spares: Vec<Slot>,
    borrowed: bool,
    borrowed_stale: Option<DamageRect>,
    stats: MailboxStats,
}

/// One ready frame and two snapshot buffers in total. A newer publication
/// replaces the unread frame and retains its damage. The producer owns its
/// authoritative framebuffer separately. Only one consumer lease may exist.
pub struct FrameMailbox {
    state: Mutex<State>,
    geometry: AtomicU32,
    pending: AtomicBool,
}

impl FrameMailbox {
    pub fn new(width: u16, height: u16) -> io::Result<Self> {
        let full = DamageRect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut spares = Vec::with_capacity(2);
        for _ in 0..2 {
            spares.push(Slot {
                framebuffer: Framebuffer::new(width, height)?,
                stale: Some(full),
            });
        }
        Ok(Self {
            state: Mutex::new(State {
                dimensions: (width, height),
                ready: None,
                spares,
                borrowed: false,
                borrowed_stale: None,
                stats: MailboxStats::default(),
            }),
            geometry: AtomicU32::new(u32::from(width) << 16 | u32::from(height)),
            pending: AtomicBool::new(false),
        })
    }

    fn state(&self) -> io::Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| io::Error::other("frame mailbox lock poisoned"))
    }

    /// Publish a coherent source snapshot and its changed area. Returns true
    /// only on the transition to a pending frame, for coalesced window wakes.
    /// Invalid regions are rejected before altering mailbox state.
    pub fn publish(
        &self,
        source: &Framebuffer,
        damage: DamageRect,
        completed: Instant,
    ) -> io::Result<bool> {
        if damage.width == 0
            || damage.height == 0
            || usize::from(damage.x) + usize::from(damage.width) > source.width()
            || usize::from(damage.y) + usize::from(damage.height) > source.height()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid published damage",
            ));
        }
        let mut state = self.state()?;
        let dimensions = (source.width, source.height);
        let resized = state.dimensions != dimensions;
        let full = DamageRect {
            x: 0,
            y: 0,
            width: dimensions.0,
            height: dimensions.1,
        };
        let damage = if resized { full } else { damage };
        for spare in &mut state.spares {
            spare.stale = if resized {
                Some(full)
            } else {
                union(spare.stale, damage)
            };
        }
        if state.borrowed {
            state.borrowed_stale = if resized {
                Some(full)
            } else {
                union(state.borrowed_stale, damage)
            };
        }
        let wake = state.ready.is_none();
        let mut ready = if let Some(mut ready) = state.ready.take() {
            ready.slot.stale = Some(damage);
            ready.damage = if resized {
                full
            } else {
                union(Some(ready.damage), damage).unwrap()
            };
            state.stats.replaced += 1;
            ready
        } else {
            let slot = state
                .spares
                .pop()
                .ok_or_else(|| io::Error::other("frame mailbox has no spare buffer"))?;
            ReadyFrame {
                slot,
                damage,
                completed,
            }
        };
        if (ready.slot.framebuffer.width, ready.slot.framebuffer.height) != dimensions {
            ready.slot.framebuffer = Framebuffer::new(dimensions.0, dimensions.1)?;
            ready.slot.stale = Some(full);
            ready.damage = full;
        }
        if let Some(area) = ready.slot.stale.take() {
            copy_area(source, &mut ready.slot.framebuffer, area);
            state.stats.copied_pixels += u64::from(area.width) * u64::from(area.height);
        }
        ready.completed = completed;
        state.dimensions = dimensions;
        state.stats.published += 1;
        state.ready = Some(ready);
        self.geometry.store(
            u32::from(dimensions.0) << 16 | u32::from(dimensions.1),
            Ordering::Release,
        );
        self.pending.store(true, Ordering::Release);
        Ok(wake)
    }

    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    pub fn stats(&self) -> io::Result<MailboxStats> {
        Ok(self.state()?.stats)
    }

    /// Input mapping reads geometry without waiting for a snapshot copy.
    pub fn dimensions(&self) -> (usize, usize) {
        let geometry = self.geometry.load(Ordering::Acquire);
        ((geometry >> 16) as usize, (geometry & 0xffff) as usize)
    }

    /// The lease keeps this snapshot immutable while the producer continues.
    /// Dropping it returns its buffer without retaining any framebuffer lock.
    pub fn take(&self) -> io::Result<Option<FrameLease<'_>>> {
        let mut state = self.state()?;
        if state.borrowed {
            return Ok(None);
        }
        let Some(ready) = state.ready.take() else {
            return Ok(None);
        };
        self.pending.store(false, Ordering::Release);
        state.borrowed = true;
        state.borrowed_stale = None;
        state.stats.consumed += 1;
        Ok(Some(FrameLease {
            mailbox: self,
            ready: Some(ready),
        }))
    }
}

pub struct FrameLease<'a> {
    mailbox: &'a FrameMailbox,
    ready: Option<ReadyFrame>,
}

impl FrameLease<'_> {
    pub fn framebuffer(&self) -> &Framebuffer {
        &self.ready.as_ref().unwrap().slot.framebuffer
    }
    pub fn damage(&self) -> DamageRect {
        self.ready.as_ref().unwrap().damage
    }
    pub fn completed(&self) -> Instant {
        self.ready.as_ref().unwrap().completed
    }
}

impl Drop for FrameLease<'_> {
    fn drop(&mut self) {
        if let (Some(mut ready), Ok(mut state)) = (self.ready.take(), self.mailbox.state.lock()) {
            ready.slot.stale = state.borrowed_stale.take();
            state.borrowed = false;
            state.spares.push(ready.slot);
            debug_assert!(state.spares.len() + usize::from(state.ready.is_some()) <= 2);
        }
    }
}

fn union(older: Option<DamageRect>, newer: DamageRect) -> Option<DamageRect> {
    let Some(older) = older else {
        return Some(newer);
    };
    let x = older.x.min(newer.x);
    let y = older.y.min(newer.y);
    Some(DamageRect {
        x,
        y,
        width: (older.x + older.width).max(newer.x + newer.width) - x,
        height: (older.y + older.height).max(newer.y + newer.height) - y,
    })
}

fn copy_area(source: &Framebuffer, target: &mut Framebuffer, area: DamageRect) {
    let stride = source.width();
    for row in usize::from(area.y)..usize::from(area.y) + usize::from(area.height) {
        let start = row * stride + usize::from(area.x);
        let end = start + usize::from(area.width);
        target.pixels[start..end].copy_from_slice(&source.pixels[start..end]);
    }
}

/// A bounded rolling distribution of submitted-frame intervals. Submissions
/// are not proof of physical display timing or input-to-screen latency.
pub struct FramePacing {
    last: Option<Instant>,
    intervals: std::collections::VecDeque<Duration>,
}

impl Default for FramePacing {
    fn default() -> Self {
        Self {
            last: None,
            intervals: std::collections::VecDeque::with_capacity(240),
        }
    }
}

impl FramePacing {
    pub fn record(&mut self, now: Instant) {
        if let Some(last) = self.last.replace(now) {
            if now.saturating_duration_since(last) > Duration::from_millis(250) {
                self.intervals.clear();
                return;
            }
            if self.intervals.len() == 240 {
                self.intervals.pop_front();
            }
            self.intervals
                .push_back(now.saturating_duration_since(last));
        }
    }

    pub fn reset(&mut self) {
        self.last = None;
        self.intervals.clear();
    }

    pub fn summary(&self) -> Option<(f64, Duration)> {
        if self.intervals.is_empty() {
            return None;
        }
        let mut intervals: Vec<_> = self.intervals.iter().copied().collect();
        intervals.sort_unstable();
        let mean = intervals.iter().sum::<Duration>().as_secs_f64() / intervals.len() as f64;
        Some((
            if mean > 0.0 { 1.0 / mean } else { 0.0 },
            intervals[intervals.len() * 95 / 100],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full(width: u16, height: u16) -> DamageRect {
        DamageRect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }
    fn point(x: u16, y: u16) -> DamageRect {
        DamageRect {
            x,
            y,
            width: 1,
            height: 1,
        }
    }

    #[test]
    fn latest_frame_keeps_skipped_damage_and_timestamp() {
        let frames = FrameMailbox::new(4, 2).unwrap();
        let mut source = Framebuffer::new(4, 2).unwrap();
        let now = Instant::now();
        assert!(frames.publish(&source, full(4, 2), now).unwrap());
        drop(frames.take().unwrap().unwrap());
        for (index, color) in [(0, 1), (7, 2), (3, 3)] {
            source.pixels_mut()[index] = color;
            let wake = frames
                .publish(
                    &source,
                    point((index % 4) as u16, (index / 4) as u16),
                    now + Duration::from_millis(u64::from(color)),
                )
                .unwrap();
            assert_eq!(wake, color == 1);
        }
        let frame = frames.take().unwrap().unwrap();
        assert_eq!(frame.framebuffer().pixels(), source.pixels());
        assert_eq!(frame.damage(), full(4, 2));
        assert_eq!(frame.completed(), now + Duration::from_millis(3));
        assert_eq!(frames.stats().unwrap().replaced, 2);
        assert!(!frames.pending());
    }

    #[test]
    fn snapshots_remain_immutable_during_overlap_and_recycle_incrementally() {
        let frames = FrameMailbox::new(4, 2).unwrap();
        let mut source = Framebuffer::new(4, 2).unwrap();
        source.pixels_mut().fill(1);
        let now = Instant::now();
        frames.publish(&source, full(4, 2), now).unwrap();
        let first = frames.take().unwrap().unwrap();
        let first_buffer = first.framebuffer().pixels().as_ptr();
        source.pixels_mut()[0] = 2;
        frames.publish(&source, point(0, 0), now).unwrap();
        source.pixels_mut()[7] = 3;
        frames.publish(&source, point(3, 1), now).unwrap();
        assert!(first.framebuffer().pixels().iter().all(|pixel| *pixel == 1));
        assert!(frames.take().unwrap().is_none());
        drop(first);
        let second = frames.take().unwrap().unwrap();
        assert_eq!(second.framebuffer().pixels(), source.pixels());
        assert_ne!(first_buffer, second.framebuffer().pixels().as_ptr());
        source.pixels_mut()[3] = 4;
        frames.publish(&source, point(3, 0), now).unwrap();
        assert_eq!(second.framebuffer().pixels()[3], 1);
        drop(second);
        let third = frames.take().unwrap().unwrap();
        assert_eq!(third.framebuffer().pixels().as_ptr(), first_buffer);
        assert_eq!(third.framebuffer().pixels(), source.pixels());
        drop(third);
        // Both buffers have now caught up. A one-pixel update patches one
        // pixel, even though it returns a complete coherent snapshot.
        let before = frames.stats().unwrap().copied_pixels;
        source.pixels_mut()[4] = 5;
        frames.publish(&source, point(0, 1), now).unwrap();
        // The other reusable slot may also need the previous one-pixel edit.
        assert!(frames.stats().unwrap().copied_pixels - before <= 8);
        assert_eq!(
            frames.take().unwrap().unwrap().framebuffer().pixels(),
            source.pixels()
        );
    }

    #[test]
    fn resizing_with_a_borrowed_frame_discards_stale_geometry() {
        let frames = FrameMailbox::new(8, 8).unwrap();
        let initial = Framebuffer::new(8, 8).unwrap();
        let now = Instant::now();
        frames.publish(&initial, full(8, 8), now).unwrap();
        let borrowed = frames.take().unwrap().unwrap();
        let mut resized = Framebuffer::new(2, 3).unwrap();
        resized.pixels_mut().fill(0xabcdef);
        frames.publish(&resized, point(1, 2), now).unwrap();
        assert_eq!(borrowed.framebuffer().width(), 8);
        drop(borrowed);
        let next = frames.take().unwrap().unwrap();
        assert_eq!(next.damage(), full(2, 3));
        assert_eq!(next.framebuffer().pixels(), resized.pixels());
        resized.pixels_mut()[0] = 17;
        frames.publish(&resized, point(0, 0), now).unwrap();
        drop(next);
        let next = frames.take().unwrap().unwrap();
        assert_eq!(next.damage(), full(2, 3));
        assert_eq!(next.framebuffer().pixels(), resized.pixels());
    }

    #[test]
    fn malformed_damage_does_not_replace_a_ready_frame() {
        let frames = FrameMailbox::new(4, 4).unwrap();
        let source = Framebuffer::new(4, 4).unwrap();
        let now = Instant::now();
        frames.publish(&source, full(4, 4), now).unwrap();
        for area in [
            full(0, 4),
            full(4, 0),
            point(4, 0),
            point(0, u16::MAX),
            full(u16::MAX, u16::MAX),
        ] {
            assert!(frames.publish(&source, area, now).is_err());
        }
        assert_eq!(frames.stats().unwrap().published, 1);
        assert_eq!(
            frames.take().unwrap().unwrap().framebuffer().pixels(),
            source.pixels()
        );
        assert!(FrameMailbox::new(0, 4).is_err());
    }

    #[test]
    fn input_geometry_never_waits_for_a_snapshot_copy() {
        let frames = std::sync::Arc::new(FrameMailbox::new(4, 3).unwrap());
        let (sender, receiver) = std::sync::mpsc::channel();
        let held_copy_lock = frames.state().unwrap();
        let input_frames = std::sync::Arc::clone(&frames);
        let input = std::thread::spawn(move || {
            sender
                .send((input_frames.dimensions(), input_frames.pending()))
                .unwrap()
        });
        let geometry = receiver.recv_timeout(Duration::from_secs(1));
        drop(held_copy_lock);
        input.join().unwrap();
        assert_eq!(geometry.unwrap(), ((4, 3), false));
    }

    #[test]
    fn a_stalled_consumer_keeps_one_ready_frame_and_reuses_two_buffers() {
        let frames = FrameMailbox::new(8, 8).unwrap();
        let mut source = Framebuffer::new(8, 8).unwrap();
        let mut pointers = std::collections::HashSet::new();
        let now = Instant::now();
        for frame in 0..1000 {
            source.pixels_mut().fill(frame);
            frames.publish(&source, full(8, 8), now).unwrap();
            if frame % 17 == 0 {
                let lease = frames.take().unwrap().unwrap();
                pointers.insert(lease.framebuffer().pixels().as_ptr());
                assert!(
                    lease
                        .framebuffer()
                        .pixels()
                        .iter()
                        .all(|pixel| *pixel == frame)
                );
            }
        }
        assert!(pointers.len() <= 2);
        let final_frame = frames.take().unwrap().unwrap();
        assert!(
            final_frame
                .framebuffer()
                .pixels()
                .iter()
                .all(|pixel| *pixel == 999)
        );
        assert_eq!(frames.stats().unwrap().published, 1000);
        assert_eq!(
            frames.stats().unwrap().replaced + frames.stats().unwrap().consumed,
            1000
        );
    }

    #[test]
    fn source_can_decode_a_new_frame_without_exposing_partial_bands() {
        let frames = FrameMailbox::new(8, 8).unwrap();
        let mut source = Framebuffer::new(8, 8).unwrap();
        let now = Instant::now();
        source.pixels_mut().fill(1);
        frames.publish(&source, full(8, 8), now).unwrap();
        let shown = frames.take().unwrap().unwrap();
        for band in 0..8 {
            source.pixels_mut()[band * 8..band * 8 + 8].fill(2);
            assert!(shown.framebuffer().pixels().iter().all(|pixel| *pixel == 1));
            assert!(!frames.pending());
        }
        frames.publish(&source, full(8, 8), now).unwrap();
        drop(shown);
        assert!(
            frames
                .take()
                .unwrap()
                .unwrap()
                .framebuffer()
                .pixels()
                .iter()
                .all(|pixel| *pixel == 2)
        );
    }

    #[test]
    fn concurrent_decode_cannot_mutate_a_consumers_snapshot() {
        let frames = std::sync::Arc::new(FrameMailbox::new(64, 64).unwrap());
        let producer_frames = std::sync::Arc::clone(&frames);
        let (notifier, notices) = std::sync::mpsc::sync_channel(1);
        let producer = std::thread::spawn(move || {
            let mut source = Framebuffer::new(64, 64).unwrap();
            for frame in 1..=500 {
                source.pixels_mut().fill(frame);
                producer_frames
                    .publish(&source, full(64, 64), Instant::now())
                    .unwrap();
                notifier.send(()).unwrap();
            }
        });
        let mut last = 0;
        for () in notices {
            if let Some(frame) = frames.take().unwrap() {
                let marker = frame.framebuffer().pixels()[0];
                assert!(marker > last);
                std::thread::yield_now();
                assert!(
                    frame
                        .framebuffer()
                        .pixels()
                        .iter()
                        .all(|pixel| *pixel == marker)
                );
                last = marker;
            }
        }
        producer.join().unwrap();
        assert_eq!(last, 500);
    }

    #[test]
    fn pacing_reports_tail_intervals_and_resets_after_idle() {
        let mut pacing = FramePacing::default();
        let now = Instant::now();
        for milliseconds in [0, 8, 16, 24, 48] {
            pacing.record(now + Duration::from_millis(milliseconds));
        }
        let (fps, p95) = pacing.summary().unwrap();
        assert!((fps - 83.333).abs() < 0.01);
        assert_eq!(p95, Duration::from_millis(24));
        pacing.record(now + Duration::from_secs(1));
        assert!(pacing.summary().is_none());
        for milliseconds in 0..1000 {
            pacing.record(now + Duration::from_secs(1) + Duration::from_millis(milliseconds));
        }
        assert!(pacing.intervals.len() <= 240);
    }
}
