//! End-to-end latency of foveated Tight between TopVNC's server and viewer
//! session, through a local proxy that emulates a link (as latency_bench).
//!
//! A producer pans a real game frame like a turning camera, at a fixed
//! rate. Each frame carries, in 16-pixel blocks that survive JPEG:
//! - its frame number in the tile left of the screen center (crosshair);
//! - the latest input sequence number the host received, right of center;
//! - its frame number again in the top-left (periphery) tile.
//!
//! The viewer side sends a pointer event every 4 ms whose position encodes
//! a sequence number, and records:
//! - center: publish -> the crosshair tiles decoded on the viewer;
//! - full: publish -> the whole update decoded;
//! - input: pointer event sent -> its sequence number visible at the
//!   crosshair, including the uplink and the wait for the next host frame;
//! - input -> full: the same, to the end of that update, which is when
//!   today's viewer presents;
//! - periphery age: how old the periphery is when an update completes.
//!
//! Modes are `off` and `on` foveation for clients at quality 6 and 3
//! (`off q6`, `on q6`, `off q3`, `on q3`).
//!
//! cargo run --release --example fovea_latency -- --frame PATH.rgb
//!   [--fps 120] [--seconds 5] [--links 1000:2,300:4,150:8,75:8]
//!   [--modes NAME,NAME] [--json OUT.jsonl] [--label TEXT]
//!
//! `specs/008-foveated-tight/rnd/fetch_frames.py` writes suitable frames.

use std::error::Error;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use topvnc::{ClientEvent, Encoding, Foveation, Framebuffer, ServerConfig, Session, VncServer};

const WIDTH: usize = 1920;
const HEIGHT: usize = 1080;
/// Camera turn per frame, in pixels of the panorama.
const PAN: usize = 23;
const CENTER_FRAME: (usize, usize) = (896, 512);
const CENTER_INPUT: (usize, usize) = (960, 512);
const PERIPHERY_FRAME: (usize, usize) = (0, 0);
const INPUT_INTERVAL: Duration = Duration::from_millis(4);
/// Link emulation granularity: about six full-size Ethernet frames.
const SLICE_BYTES: usize = 8 * 1024;

struct Scene {
    panorama: Vec<u32>,
}

impl Scene {
    /// The frame and its mirror side by side, so panning wraps seamlessly.
    fn new(frame: &[u32]) -> Self {
        let mut panorama = Vec::with_capacity(WIDTH * 2 * HEIGHT);
        for y in 0..HEIGHT {
            let row = &frame[y * WIDTH..(y + 1) * WIDTH];
            panorama.extend_from_slice(row);
            panorama.extend(row.iter().rev());
        }
        Self { panorama }
    }

    fn render(&self, frame: u32, input: u32, framebuffer: &mut Framebuffer) {
        let offset = frame as usize * PAN % (WIDTH * 2);
        let pixels = framebuffer.pixels_mut();
        for y in 0..HEIGHT {
            let source = &self.panorama[y * WIDTH * 2..(y + 1) * WIDTH * 2];
            let first = (WIDTH * 2 - offset).min(WIDTH);
            let target = &mut pixels[y * WIDTH..(y + 1) * WIDTH];
            target[..first].copy_from_slice(&source[offset..offset + first]);
            target[first..].copy_from_slice(&source[..WIDTH - first]);
        }
        marker(frame, pixels, CENTER_FRAME);
        marker(input, pixels, CENTER_INPUT);
        marker(frame, pixels, PERIPHERY_FRAME);
    }
}

/// `value` as a 4x4 grid of black and white 16-pixel blocks, least
/// significant bit first.
fn marker(value: u32, pixels: &mut [u32], (x, y): (usize, usize)) {
    for bit in 0..16 {
        let color = if value >> bit & 1 != 0 { 0xffffff } else { 0 };
        let (bx, by) = (x + bit % 4 * 16, y + bit / 4 * 16);
        for row in by..by + 16 {
            pixels[row * WIDTH + bx..row * WIDTH + bx + 16].fill(color);
        }
    }
}

fn read_marker(framebuffer: &Framebuffer, (x, y): (usize, usize)) -> u32 {
    let pixels = framebuffer.pixels();
    (0..16)
        .map(|bit| {
            let (px, py) = (x + bit % 4 * 16 + 8, y + bit / 4 * 16 + 8);
            u32::from(pixels[py * WIDTH + px] >> 8 & 0xff >= 128) << bit
        })
        .sum()
}

fn covers(rect: (usize, usize, usize, usize), (x, y): (usize, usize)) -> bool {
    rect.0 <= x && x + 64 <= rect.0 + rect.2 && rect.1 <= y && y + 64 <= rect.1 + rect.3
}

/// Forward `from` to `to`, delaying each chunk by `delay` and pacing the
/// stream to `bits_per_second` when given (from latency_bench).
fn pipe(
    mut from: TcpStream,
    mut to: TcpStream,
    delay: Duration,
    bits_per_second: Option<f64>,
    counter: Arc<AtomicU64>,
) {
    let (sender, receiver) = mpsc::channel::<(Instant, Vec<u8>)>();
    let reader = thread::spawn(move || {
        let mut buffer = vec![0; 64 * 1024];
        while let Ok(count) = from.read(&mut buffer) {
            if count == 0
                || sender
                    .send((Instant::now(), buffer[..count].to_vec()))
                    .is_err()
            {
                break;
            }
        }
    });
    let mut busy_until = Instant::now();
    'chunks: for (arrived, chunk) in receiver {
        let due = arrived + delay;
        let now = Instant::now();
        if due > now {
            thread::sleep(due - now);
        }
        // Forward in slices, each when its last byte has crossed the link,
        // so the first bytes of a large read are not held back until the
        // whole read has (latency_bench forwards whole reads of up to 64 KB).
        for slice in chunk.chunks(SLICE_BYTES) {
            if let Some(rate) = bits_per_second {
                let now = Instant::now();
                if now > busy_until + Duration::from_millis(2) {
                    busy_until = now;
                }
                busy_until += Duration::from_secs_f64(slice.len() as f64 * 8.0 / rate);
                if busy_until > now + Duration::from_micros(150) {
                    thread::sleep(busy_until - now);
                }
            }
            if to.write_all(slice).is_err() {
                break 'chunks;
            }
            counter.fetch_add(slice.len() as u64, Ordering::Relaxed);
        }
    }
    let _ = to.shutdown(Shutdown::Write);
    let _ = reader.join();
}

fn start_link(
    server: std::net::SocketAddr,
    delay: Duration,
    mbps: f64,
) -> io::Result<(std::net::SocketAddr, Arc<AtomicU64>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let downstream = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&downstream);
    thread::spawn(move || {
        let Ok((client, _)) = listener.accept() else {
            return;
        };
        let Ok(upstream) = TcpStream::connect(server) else {
            return;
        };
        let _ = client.set_nodelay(true);
        let _ = upstream.set_nodelay(true);
        let (client_read, upstream_write) =
            (client.try_clone().unwrap(), upstream.try_clone().unwrap());
        thread::spawn(move || {
            pipe(
                client_read,
                upstream_write,
                delay,
                None,
                Arc::new(AtomicU64::new(0)),
            )
        });
        pipe(upstream, client, delay, Some(mbps * 1e6), counter);
    });
    Ok((address, downstream))
}

#[derive(Clone)]
struct Mode {
    name: &'static str,
    quality: u8,
    foveation: Foveation,
}

fn modes() -> Vec<Mode> {
    vec![
        Mode {
            name: "off q6",
            quality: 6,
            foveation: Foveation::Off,
        },
        Mode {
            name: "on q6",
            quality: 6,
            foveation: Foveation::On,
        },
        Mode {
            name: "off q3",
            quality: 3,
            foveation: Foveation::Off,
        },
        Mode {
            name: "on q3",
            quality: 3,
            foveation: Foveation::On,
        },
    ]
}

#[derive(Default)]
struct Stats {
    center: Vec<f64>,
    full: Vec<f64>,
    input: Vec<f64>,
    /// Input to the end of the update that first showed it: when today's
    /// viewer presents.
    input_full: Vec<f64>,
    periphery_age: Vec<f64>,
    frames: usize,
    bytes: u64,
}

fn percentile(values: &mut [f64], p: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * p).round() as usize]
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len().max(1) as f64
}

fn run(
    scene: &Arc<Scene>,
    mode: &Mode,
    fps: f64,
    seconds: f64,
    mbps: f64,
    delay: Duration,
) -> Result<Stats, Box<dyn Error>> {
    let mut initial = Framebuffer::new(WIDTH as u16, HEIGHT as u16)?;
    scene.render(1, 0, &mut initial);
    let server = Arc::new(VncServer::bind(
        "127.0.0.1:0",
        initial.clone(),
        ServerConfig {
            allow_insecure: true,
            foveation: mode.foveation,
            ..ServerConfig::default()
        },
    )?);
    let runner = Arc::clone(&server);
    thread::spawn(move || runner.run());
    let (link, downstream) = start_link(server.local_addr()?, delay, mbps)?;

    let published = Arc::new(Mutex::new(vec![Instant::now(); 2]));
    let stop = Arc::new(AtomicBool::new(false));
    let producer = {
        let (server, scene, published, stop) = (
            Arc::clone(&server),
            Arc::clone(scene),
            Arc::clone(&published),
            Arc::clone(&stop),
        );
        let interval = Duration::from_secs_f64(1.0 / fps);
        let mut framebuffer = initial;
        thread::spawn(move || {
            let start = Instant::now();
            let mut frame = 2u32;
            let mut input = 0u32;
            while !stop.load(Ordering::Acquire) {
                // Like a game reading input at the start of its frame.
                while let Ok(event) = server.try_event() {
                    if let ClientEvent::Pointer { x, y, .. } = event {
                        input = input.max(u32::from(y) * WIDTH as u32 + u32::from(x));
                    }
                }
                scene.render(frame, input & 0xffff, &mut framebuffer);
                published.lock().unwrap().push(Instant::now());
                let _ = server.update_framebuffer(&framebuffer);
                let next = start + interval * (frame - 1);
                let now = Instant::now();
                if next > now {
                    thread::sleep(next - now);
                }
                frame += 1;
            }
        })
    };

    let mut session = Session::connect_with_encoding(
        &link.to_string(),
        true,
        Encoding::Tight {
            quality: mode.quality,
        },
        || unreachable!("no authentication"),
    )?;
    session.set_continuous_updates(true);
    let writer = session.writer();
    // Pointer events whose position encodes a sequence number.
    let sends = Arc::new(Mutex::new(vec![Instant::now()]));
    let sender_stop = Arc::new(AtomicBool::new(false));
    let next_sequence = Arc::new(AtomicU32::new(1));
    let input_thread = {
        let (writer, sends, stop, next_sequence) = (
            writer.clone(),
            Arc::clone(&sends),
            Arc::clone(&sender_stop),
            Arc::clone(&next_sequence),
        );
        thread::spawn(move || {
            let start = Instant::now();
            let mut sequence = 1u32;
            while !stop.load(Ordering::Acquire) && sequence < 0xffff {
                sends.lock().unwrap().push(Instant::now());
                next_sequence.store(sequence + 1, Ordering::Release);
                let (x, y) = (sequence as usize % WIDTH, sequence as usize / WIDTH);
                if writer.pointer(0, x as u16, y as u16).is_err() {
                    break;
                }
                sequence += 1;
                let next = start + INPUT_INTERVAL * sequence;
                let now = Instant::now();
                if next > now {
                    thread::sleep(next - now);
                }
            }
        })
    };

    let mut received = Framebuffer::new(WIDTH as u16, HEIGHT as u16)?;
    let mut scratch = Vec::new();
    let mut stats = Stats::default();
    let warmup = Instant::now() + Duration::from_secs(1);
    let end = warmup + Duration::from_secs_f64(seconds);
    let mut bytes_at_warmup = None;
    let mut last_frame = 0;
    let mut last_input = 0u32;
    // The newest frame the viewer's periphery shows.
    let mut periphery_frame = 0u32;
    writer.request_update(false, WIDTH as u16, HEIGHT as u16)?;
    while Instant::now() < end {
        let mut center_frame: Option<Instant> = None;
        let mut center_input: Option<Instant> = None;
        let mut periphery: Option<u32> = None;
        session.read_update_pipelined(&mut scratch, |x, y, w, h, bytes| {
            received.apply_raw(x, y, w, h, bytes)?;
            let rect = (
                usize::from(x),
                usize::from(y),
                usize::from(w),
                usize::from(h),
            );
            if covers(rect, CENTER_FRAME) {
                center_frame = Some(Instant::now());
            }
            if covers(rect, CENTER_INPUT) {
                center_input = Some(Instant::now());
            }
            if covers(rect, PERIPHERY_FRAME) {
                periphery = Some(read_marker(&received, PERIPHERY_FRAME));
            }
            Ok(())
        })?;
        let now = Instant::now();
        if let Some(shown) = periphery {
            periphery_frame = periphery_frame.max(shown);
        }
        let (Some(center_frame), Some(center_input)) = (center_frame, center_input) else {
            continue;
        };
        let center = center_frame.max(center_input);
        let frame = read_marker(&received, CENTER_FRAME);
        let input = read_marker(&received, CENTER_INPUT);
        if now < warmup {
            last_frame = frame;
            last_input = input;
            continue;
        }
        bytes_at_warmup.get_or_insert_with(|| downstream.load(Ordering::Relaxed));
        if frame == last_frame {
            continue;
        }
        last_frame = frame;
        stats.frames += 1;
        let published = published.lock().unwrap();
        if let Some(at) = published.get(frame as usize) {
            stats.center.push((center - *at).as_secs_f64() * 1000.0);
            stats.full.push((now - *at).as_secs_f64() * 1000.0);
        }
        if let Some(at) = published.get(periphery_frame as usize) {
            stats.periphery_age.push((now - *at).as_secs_f64() * 1000.0);
        }
        drop(published);
        // Every input sequence shown for the first time.
        if input > last_input && input < next_sequence.load(Ordering::Acquire) {
            let sends = sends.lock().unwrap();
            for sequence in last_input + 1..=input {
                if let Some(at) = sends.get(sequence as usize) {
                    stats.input.push((center - *at).as_secs_f64() * 1000.0);
                    stats.input_full.push((now - *at).as_secs_f64() * 1000.0);
                }
            }
            last_input = input;
        }
    }
    stats.bytes = downstream.load(Ordering::Relaxed) - bytes_at_warmup.unwrap_or(0);
    sender_stop.store(true, Ordering::Release);
    stop.store(true, Ordering::Release);
    input_thread.join().unwrap();
    producer.join().unwrap();
    let _ = writer.shutdown();
    server.stop();
    Ok(stats)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut frame_path = None;
    let mut fps = 120.0;
    let mut seconds = 5.0;
    let mut links = vec![(1000.0, 2u64), (300.0, 4), (150.0, 8), (75.0, 8)];
    let mut selected: Option<Vec<String>> = None;
    let mut json = None;
    let mut label = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--frame" => frame_path = Some(value()?),
            "--fps" => fps = value()?.parse()?,
            "--seconds" => seconds = value()?.parse()?,
            "--links" => {
                links = value()?
                    .split(',')
                    .map(|link| {
                        let (mbps, delay) = link.split_once(':').expect("MBPS:DELAY_MS");
                        (mbps.parse().unwrap(), delay.parse().unwrap())
                    })
                    .collect()
            }
            "--modes" => selected = Some(value()?.split(',').map(String::from).collect()),
            "--json" => json = Some(value()?),
            "--label" => label = value()?,
            _ => return Err(format!("unknown argument {arg}").into()),
        }
    }
    let path = frame_path.ok_or("--frame PATH.rgb is required")?;
    let bytes = std::fs::read(&path)?;
    let frame: Vec<u32> = bytes
        .chunks_exact(3)
        .map(|p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]))
        .collect();
    assert_eq!(frame.len(), WIDTH * HEIGHT);
    let scene = Arc::new(Scene::new(&frame));
    let mut out = json
        .map(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        })
        .transpose()?;
    println!("{path}: {fps} fps, {seconds} s per row, pan {PAN} px/frame");
    for (mbps, delay_ms) in links {
        println!("Link {mbps} Mbit/s, {delay_ms} ms each way");
        println!(
            "  {:<8} {:>6} {:>7} | {:>13} | {:>13} | {:>13} | {:>13} | {:>6}",
            "mode",
            "fps",
            "KB/frm",
            "center mean/95",
            "full mean/95",
            "input mean/95",
            "in>full m/95",
            "periph"
        );
        for mode in modes() {
            if selected
                .as_ref()
                .is_some_and(|names| !names.iter().any(|name| name == mode.name))
            {
                continue;
            }
            let mut stats = run(
                &scene,
                &mode,
                fps,
                seconds,
                mbps,
                Duration::from_millis(delay_ms),
            )?;
            let frames = stats.frames.max(1) as f64;
            let (c, f, i, p) = (
                mean(&stats.center),
                mean(&stats.full),
                mean(&stats.input),
                mean(&stats.periphery_age),
            );
            let i_full = mean(&stats.input_full);
            let i_full95 = percentile(&mut stats.input_full, 0.95);
            let (c95, f95, i95) = (
                percentile(&mut stats.center, 0.95),
                percentile(&mut stats.full, 0.95),
                percentile(&mut stats.input, 0.95),
            );
            let i50 = percentile(&mut stats.input, 0.5);
            let kb = stats.bytes as f64 / frames / 1000.0;
            println!(
                "  {:<8} {:>6.1} {:>7.1} | {:>6.1} {:>6.1} | {:>6.1} {:>6.1} | {:>6.1} {:>6.1} | {:>6.1} {:>6.1} | {:>6.1}",
                mode.name,
                frames / seconds,
                kb,
                c,
                c95,
                f,
                f95,
                i,
                i95,
                i_full,
                i_full95,
                p
            );
            if let Some(out) = &mut out {
                writeln!(
                    out,
                    "{{\"label\":\"{label}\",\"frame\":\"{path}\",\"fps_source\":{fps},\"mbps\":{mbps},\"delay_ms\":{delay_ms},\"mode\":\"{}\",\"fps\":{:.2},\"kb_per_frame\":{kb:.1},\"mbit_s\":{:.1},\"center_mean\":{c:.2},\"center_p95\":{c95:.2},\"full_mean\":{f:.2},\"full_p95\":{f95:.2},\"input_mean\":{i:.2},\"input_p50\":{i50:.2},\"input_p95\":{i95:.2},\"input_full_mean\":{i_full:.2},\"input_full_p95\":{i_full95:.2},\"periphery_age\":{p:.2}}}",
                    mode.name,
                    frames / seconds,
                    stats.bytes as f64 * 8.0 / seconds / 1e6,
                )?;
            }
        }
    }
    Ok(())
}
