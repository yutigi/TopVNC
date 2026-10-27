//! Exercise the real incremental mailbox with a fast producer and slower
//! consumer. This CPU handoff benchmark has no network, codec, GPU or display.
//! `cargo run --release --example frame_pacing_bench -- [--stall-ms 50]`
use std::error::Error;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use topvnc::presentation::FrameMailbox;
use topvnc::{DamageRect, Framebuffer};

#[cfg(windows)]
struct Timer;
#[cfg(windows)]
impl Timer {
    fn new() -> Self {
        unsafe {
            windows_sys::Win32::Media::timeBeginPeriod(1);
        }
        Self
    }
}
#[cfg(windows)]
impl Drop for Timer {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Media::timeEndPeriod(1);
        }
    }
}

fn summarize(values: &mut [Duration]) -> (f64, f64) {
    values.sort_unstable();
    if values.is_empty() {
        return (0.0, 0.0);
    }
    (
        values.iter().sum::<Duration>().as_secs_f64() * 1000.0 / values.len() as f64,
        values[values.len() * 95 / 100].as_secs_f64() * 1000.0,
    )
}

fn main() -> Result<(), Box<dyn Error>> {
    let (width, height) = (1920u16, 1080u16);
    let mut seconds = 5u64;
    let mut stall = Duration::ZERO;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        let value: u64 = args.next().ok_or("option needs a number")?.parse()?;
        match argument.as_str() {
            "--seconds" if (1..=60).contains(&value) => seconds = value,
            "--stall-ms" if value <= 1000 => stall = Duration::from_millis(value),
            _ => return Err("use --seconds 1-60 and --stall-ms 0-1000".into()),
        }
    }
    #[cfg(windows)]
    let _timer = Timer::new();
    let frames = Arc::new(FrameMailbox::new(width, height)?);
    let producer_frames = Arc::clone(&frames);
    let stop = Arc::new(AtomicBool::new(false));
    let producer_stop = Arc::clone(&stop);
    let producer = thread::spawn(move || {
        let mut source = Framebuffer::new(width, height).unwrap();
        let region = DamageRect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let interval = Duration::from_secs_f64(1.0 / 240.0);
        let mut next = Instant::now();
        let mut frame = 1u32;
        while !producer_stop.load(Ordering::Acquire) {
            source.pixels_mut().fill(frame);
            producer_frames
                .publish(&source, region, Instant::now())
                .unwrap();
            frame += 1;
            next += interval;
            let now = Instant::now();
            if next < now {
                next = now;
            }
            thread::sleep(next.saturating_duration_since(now));
        }
    });
    let interval = Duration::from_secs_f64(1.0 / 120.0);
    let started = Instant::now();
    let warmup = started + Duration::from_secs(1);
    let end = warmup + Duration::from_secs(seconds);
    let mut next = started;
    let mut ages = Vec::new();
    let mut intervals = Vec::new();
    let mut last = None;
    let mut mixed = 0u64;
    let mut last_frame = 0;
    let mut borrowed = std::collections::HashSet::new();
    while Instant::now() < end {
        if let Some(frame) = frames.take()? {
            let pixels = frame.framebuffer().pixels();
            let marker = pixels[0];
            if marker <= last_frame || pixels.iter().any(|pixel| *pixel != marker) {
                mixed += 1;
            }
            last_frame = marker;
            borrowed.insert(pixels.as_ptr());
            let now = Instant::now();
            if now >= warmup {
                ages.push(now.saturating_duration_since(frame.completed()));
                if let Some(previous) = last.replace(now) {
                    intervals.push(now - previous);
                }
                if !stall.is_zero() && ages.len() % 120 == 0 {
                    thread::sleep(stall);
                }
            }
        }
        next += interval;
        let now = Instant::now();
        if next < now {
            next = now + interval;
        }
        thread::sleep(next.saturating_duration_since(now));
    }
    stop.store(true, Ordering::Release);
    producer
        .join()
        .map_err(|_| "producer stopped unexpectedly")?;
    let stats = frames.stats()?;
    let delivered = ages.len() as f64 / seconds as f64;
    let (age, age_p95) = summarize(&mut ages);
    let (gap, gap_p95) = summarize(&mut intervals);
    let maximum_age = ages.last().copied().unwrap_or_default().as_secs_f64() * 1000.0;
    let maximum_gap = intervals.last().copied().unwrap_or_default().as_secs_f64() * 1000.0;
    println!(
        "CPU mailbox only: 1920x1080, 240 Hz producer, 120 Hz consumer, {seconds}s measured + 1s warmup, {}ms periodic stall",
        stall.as_millis()
    );
    println!(
        "delivered {delivered:.1} fps; handoff age mean {age:.3} ms / p95 {age_p95:.3} ms / max {maximum_age:.3} ms; interval mean {gap:.3} ms / p95 {gap_p95:.3} ms / max {maximum_gap:.3} ms"
    );
    println!(
        "published {}; consumed {}; replaced {}; mixed/stale {}; snapshot allocations {}; copied {:.1} MiB",
        stats.published,
        stats.consumed,
        stats.replaced,
        mixed,
        borrowed.len(),
        stats.copied_pixels as f64 * 4.0 / 1024.0 / 1024.0
    );
    if mixed != 0 || borrowed.len() > 2 || ages.is_empty() {
        return Err("mailbox coherence or buffer bound failed".into());
    }
    Ok(())
}
