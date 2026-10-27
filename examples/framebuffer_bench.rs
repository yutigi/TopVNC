//! Compare the previous indexed conversion with validated row conversion.
//! `cargo run --release --example framebuffer_bench`
//! CPU framebuffer application only: excludes network, codecs, GPU, and display.

use std::hint::black_box;
use std::time::{Duration, Instant};
use topvnc::Framebuffer;

fn indexed_apply(framebuffer: &mut Framebuffer, bytes: &[u8]) {
    let (width, height) = (framebuffer.width(), framebuffer.height());
    let pixels = framebuffer.pixels_mut();
    for row in 0..height {
        let start = row * width;
        let source_start = row * width * 4;
        for column in 0..width {
            let pixel = &bytes[source_start + column * 4..source_start + column * 4 + 4];
            pixels[start + column] =
                u32::from(pixel[2]) << 16 | u32::from(pixel[1]) << 8 | u32::from(pixel[0]);
        }
    }
}

fn measure(mut apply: impl FnMut()) -> (Duration, Duration) {
    for _ in 0..50 {
        apply();
    }
    let mut samples = Vec::with_capacity(300);
    for _ in 0..300 {
        let start = Instant::now();
        apply();
        samples.push(start.elapsed());
    }
    samples.sort();
    (
        samples.iter().sum::<Duration>() / samples.len() as u32,
        samples[samples.len() * 95 / 100],
    )
}

fn main() -> std::io::Result<()> {
    println!("CPU framebuffer application only; not input-to-screen latency.");
    for (width, height) in [(1280u16, 720u16), (1920, 1080)] {
        // Include nonzero padding bytes and an unaligned source slice.
        let bytes: Vec<u8> = (0..usize::from(width) * usize::from(height) * 4 + 1)
            .map(|index| index.wrapping_mul(73) as u8)
            .collect();
        let source = &bytes[1..];
        let mut old = Framebuffer::new(width, height)?;
        let mut new = old.clone();
        indexed_apply(&mut old, source);
        new.apply_raw(0, 0, width, height, source)?;
        assert_eq!(old.pixels(), new.pixels());
        let (old_mean, old_p95) = measure(|| {
            indexed_apply(black_box(&mut old), black_box(source));
            black_box(old.pixels());
        });
        let (new_mean, new_p95) = measure(|| {
            new.apply_raw(0, 0, width, height, black_box(source))
                .unwrap();
            black_box(new.pixels());
        });
        println!(
            "{width}x{height}: indexed mean {:.3} ms, p95 {:.3} ms; row slices mean {:.3} ms, p95 {:.3} ms",
            old_mean.as_secs_f64() * 1000.0,
            old_p95.as_secs_f64() * 1000.0,
            new_mean.as_secs_f64() * 1000.0,
            new_p95.as_secs_f64() * 1000.0,
        );
    }
    Ok(())
}
