//! End-to-end frame rate and latency benchmark for TopVNC's server and client.
//!
//! A producer thread publishes a continuously panning, game-like scene to the
//! embedded server at a fixed rate. A TopVNC client session reads it through
//! a local TCP proxy that limits bandwidth and adds one-way delay, emulating
//! a network link. Every frame carries its number in black and white blocks
//! at the top-left corner, which survive JPEG, so the client knows which
//! frame each update completes.
//!
//! Reported per mode and link:
//! - delivered: distinct frames the client completed per second;
//! - latency: time from publishing a frame on the server to the client
//!   having all of it (server diff and encode, network, decode); capture and
//!   display are not included;
//! - Mbit/s and KB/frame: server-to-client traffic.
//!
//! Run with `cargo run --release --example latency_bench -- [--size WxH]
//! [--seconds N] [--fps N] [--delay-ms N] [--mbps N,N,...]`.
//!
//! `--serve HOST:PORT` instead serves the scene without authentication until
//! interrupted, for trying a viewer against moving content:
//! `topvnc HOST:PORT --allow-insecure`.

use std::error::Error;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use topvnc::{Encoding, Framebuffer, ServerConfig, Session, VncServer};

struct Options {
    width: u16,
    height: u16,
    seconds: f64,
    fps: f64,
    delay: Duration,
    links_mbps: Vec<f64>,
    /// Serve the scene at this address for a viewer instead of measuring.
    serve: Option<String>,
}

fn options() -> Result<Options, Box<dyn Error>> {
    let mut options = Options {
        width: 1920,
        height: 1080,
        seconds: 6.0,
        fps: 60.0,
        delay: Duration::from_millis(2),
        links_mbps: vec![100.0, 1000.0],
        serve: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--size" => {
                let value = value()?;
                let (width, height) = value.split_once('x').ok_or("--size is WIDTHxHEIGHT")?;
                options.width = width.parse()?;
                options.height = height.parse()?;
            }
            "--seconds" => options.seconds = value()?.parse()?,
            "--fps" => options.fps = value()?.parse()?,
            "--delay-ms" => options.delay = Duration::from_millis(value()?.parse()?),
            "--serve" => options.serve = Some(value()?),
            "--mbps" => {
                options.links_mbps = value()?
                    .split(',')
                    .map(str::parse)
                    .collect::<Result<_, _>>()?
            }
            _ => return Err(format!("unknown argument {arg}").into()),
        }
    }
    Ok(options)
}

/// A wide, smooth, textured landscape; frames pan across it.
struct Scene {
    width: usize,
    height: usize,
    texture: Vec<u32>,
}

impl Scene {
    fn new(width: usize, height: usize) -> Self {
        let texture_width = width * 2;
        let mut seed = 0x9e37_79b9u32;
        let mut texture = Vec::with_capacity(texture_width * height);
        for y in 0..height {
            for x in 0..texture_width {
                let (fx, fy) = (x as f32, y as f32);
                let horizon = height as f32 * (0.45 + 0.08 * (fx * 0.004).sin());
                // Fine grain, like texture detail in a game scene.
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let grain = (seed % 13) as f32 - 6.0;
                let (red, green, blue) = if fy < horizon {
                    let t = fy / horizon;
                    let cloud = 18.0 * ((fx * 0.011).sin() * (fy * 0.03).cos());
                    (
                        90.0 + 60.0 * t + cloud,
                        140.0 + 50.0 * t + cloud,
                        230.0 - 20.0 * t,
                    )
                } else {
                    let t = (fy - horizon) / (height as f32 - horizon);
                    let rows = 25.0 * ((fx * 0.05 + fy * 0.21).sin() * (fy * 0.07).cos());
                    (
                        70.0 + 40.0 * t + rows,
                        120.0 + 30.0 * t + rows,
                        50.0 + 20.0 * t + rows * 0.5,
                    )
                };
                let channel = |value: f32| (value + grain).clamp(0.0, 255.0) as u32;
                texture.push(channel(red) << 16 | channel(green) << 8 | channel(blue));
            }
        }
        Self {
            width,
            height,
            texture,
        }
    }

    fn render(&self, frame: u32, framebuffer: &mut Framebuffer) {
        let texture_width = self.width * 2;
        let offset = (frame as usize * 7) % self.width;
        let pixels = framebuffer.pixels_mut();
        for y in 0..self.height {
            let source = y * texture_width + offset;
            pixels[y * self.width..(y + 1) * self.width]
                .copy_from_slice(&self.texture[source..source + self.width]);
        }
        // The frame number as a 4x4 grid of black and white 16x16 blocks,
        // which survives JPEG, so the client can read it back.
        for bit in 0..FRAME_BITS {
            let color = if frame >> bit & 1 != 0 { 0xffffff } else { 0 };
            let (block_x, block_y) = (bit % 4 * 16, bit / 4 * 16);
            for y in block_y..block_y + 16 {
                pixels[y * self.width + block_x..y * self.width + block_x + 16].fill(color);
            }
        }
    }
}

const FRAME_BITS: usize = 16;

/// Read the frame number `Scene::render` drew, from the block centers.
fn frame_number(framebuffer: &Framebuffer) -> u32 {
    let pixels = framebuffer.pixels();
    (0..FRAME_BITS)
        .map(|bit| {
            let (x, y) = (bit % 4 * 16 + 8, bit / 4 * 16 + 8);
            let green = pixels[y * framebuffer.width() + x] >> 8 & 0xff;
            u32::from(green >= 128) << bit
        })
        .sum()
}

/// Forward `from` to `to`, delaying each chunk by `delay` and pacing the
/// stream to `bits_per_second` when given. Returns the forwarded byte count.
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
    // When the link finishes what it has accepted so far. Idle time earns
    // no credit, so bursts never exceed the link rate.
    let mut busy_until = Instant::now();
    for (arrived, chunk) in receiver {
        let due = arrived + delay;
        let now = Instant::now();
        if due > now {
            thread::sleep(due - now);
        }
        if let Some(rate) = bits_per_second {
            let now = Instant::now();
            // A gap longer than sleep overshoot means the link went idle.
            if now > busy_until + Duration::from_millis(2) {
                busy_until = now;
            }
            busy_until += Duration::from_secs_f64(chunk.len() as f64 * 8.0 / rate);
            // The chunk is fully across once the link has sent it.
            if busy_until > now {
                thread::sleep(busy_until - now);
            }
        }
        if to.write_all(&chunk).is_err() {
            break;
        }
        counter.fetch_add(chunk.len() as u64, Ordering::Relaxed);
    }
    let _ = to.shutdown(Shutdown::Write);
    let _ = reader.join();
}

/// Accept one client and forward it to `server`, limiting the downstream.
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

/// How the client gets each next update.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Requests {
    /// Request after each update arrives, as TopVNC 0.2.0 did.
    AfterEach,
    /// `Session::read_update_pipelined` without continuous updates.
    Adaptive,
    /// Continuous updates: the server pushes frames, with fence flow control.
    Push,
}

struct Mode {
    name: &'static str,
    encoding: Encoding,
    requests: Requests,
}

struct Outcome {
    delivered_fps: f64,
    mean_latency: Duration,
    p95_latency: Duration,
    mbit_per_second: f64,
    kb_per_frame: f64,
}

fn run(
    options: &Options,
    scene: &Arc<Scene>,
    mode: &Mode,
    mbps: f64,
) -> Result<Outcome, Box<dyn Error>> {
    let mut initial = Framebuffer::new(options.width, options.height)?;
    scene.render(1, &mut initial);
    let server = Arc::new(VncServer::bind(
        "127.0.0.1:0",
        initial.clone(),
        ServerConfig {
            allow_insecure: true,
            ..ServerConfig::default()
        },
    )?);
    let runner = Arc::clone(&server);
    thread::spawn(move || runner.run());
    let (link, downstream) = start_link(server.local_addr()?, options.delay, mbps)?;

    // Publish times by frame number.
    let published = Arc::new(Mutex::new(vec![Instant::now(); 2]));
    let stop = Arc::new(AtomicBool::new(false));
    let producer = {
        let (server, scene, published, stop) = (
            Arc::clone(&server),
            Arc::clone(scene),
            Arc::clone(&published),
            Arc::clone(&stop),
        );
        let interval = Duration::from_secs_f64(1.0 / options.fps);
        let mut framebuffer = initial;
        thread::spawn(move || {
            let start = Instant::now();
            let mut frame = 2u32;
            while !stop.load(Ordering::Acquire) {
                scene.render(frame, &mut framebuffer);
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

    let mut session =
        Session::connect_with_encoding(&link.to_string(), true, mode.encoding, || {
            unreachable!("the benchmark server uses no authentication")
        })?;
    session.set_continuous_updates(mode.requests == Requests::Push);
    let writer = session.writer();
    let (width, height) = (options.width, options.height);
    let mut received = Framebuffer::new(width, height)?;
    let mut scratch = Vec::new();
    let mut latencies = Vec::new();
    let mut last_frame = 0;
    // Skip the first second: the full initial frame and TCP ramp-up.
    let warmup = Instant::now() + Duration::from_secs(1);
    let end = warmup + Duration::from_secs_f64(options.seconds);
    let mut bytes_at_warmup = None;
    writer.request_update(false, width, height)?;
    while Instant::now() < end {
        let apply = |x, y, w, h, bytes: &[u8]| received.apply_raw(x, y, w, h, bytes);
        if mode.requests != Requests::AfterEach {
            session.read_update_pipelined(&mut scratch, apply)?;
        } else {
            session.read_update_with(&mut scratch, apply)?;
            writer.request_update(true, width, height)?;
        }
        let now = Instant::now();
        let frame = frame_number(&received);
        if now < warmup {
            last_frame = frame;
            continue;
        }
        bytes_at_warmup.get_or_insert_with(|| downstream.load(Ordering::Relaxed));
        if frame != last_frame {
            last_frame = frame;
            if let Some(published) = published.lock().unwrap().get(frame as usize) {
                latencies.push(now - *published);
            }
        }
    }
    let bytes = downstream.load(Ordering::Relaxed) - bytes_at_warmup.unwrap_or(0);
    stop.store(true, Ordering::Release);
    producer.join().unwrap();
    let _ = writer.shutdown();
    server.stop();

    latencies.sort();
    let frames = latencies.len().max(1);
    let mean = latencies.iter().sum::<Duration>() / frames as u32;
    let p95 = latencies
        .get(latencies.len() * 95 / 100)
        .copied()
        .unwrap_or_default();
    Ok(Outcome {
        delivered_fps: latencies.len() as f64 / options.seconds,
        mean_latency: mean,
        p95_latency: p95,
        mbit_per_second: bytes as f64 * 8.0 / options.seconds / 1e6,
        kb_per_frame: bytes as f64 / frames as f64 / 1000.0,
    })
}

/// Serve the panning scene until the process is stopped.
fn serve_scene(options: &Options, scene: &Scene, address: &str) -> Result<(), Box<dyn Error>> {
    let mut framebuffer = Framebuffer::new(options.width, options.height)?;
    scene.render(1, &mut framebuffer);
    let server = Arc::new(VncServer::bind(
        address,
        framebuffer.clone(),
        ServerConfig {
            allow_insecure: true,
            ..ServerConfig::default()
        },
    )?);
    let runner = Arc::clone(&server);
    thread::spawn(move || runner.run());
    println!(
        "Serving a {}x{} scene at {} fps on {} without authentication.",
        options.width,
        options.height,
        options.fps,
        server.local_addr()?
    );
    let interval = Duration::from_secs_f64(1.0 / options.fps);
    let start = Instant::now();
    for frame in 2u32.. {
        scene.render(frame, &mut framebuffer);
        server.update_framebuffer(&framebuffer)?;
        // Drain input so viewers never stall on a full event queue.
        while server.try_event().is_ok() {}
        let next = start + interval * (frame - 1);
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = options()?;
    let scene = Arc::new(Scene::new(
        usize::from(options.width),
        usize::from(options.height),
    ));
    if let Some(address) = &options.serve {
        return serve_scene(&options, &scene, address);
    }
    let modes = [
        Mode {
            name: "Raw, request after each update (0.2.0)",
            encoding: Encoding::Raw,
            requests: Requests::AfterEach,
        },
        Mode {
            name: "Raw, push",
            encoding: Encoding::Raw,
            requests: Requests::Push,
        },
        Mode {
            name: "Tight JPEG quality 9, push",
            encoding: Encoding::Tight { quality: 9 },
            requests: Requests::Push,
        },
        Mode {
            name: "Tight JPEG quality 6, request after each",
            encoding: Encoding::Tight { quality: 6 },
            requests: Requests::AfterEach,
        },
        Mode {
            name: "Tight JPEG quality 6, adaptive pipelining",
            encoding: Encoding::Tight { quality: 6 },
            requests: Requests::Adaptive,
        },
        Mode {
            name: "Tight JPEG quality 6, push",
            encoding: Encoding::Tight { quality: 6 },
            requests: Requests::Push,
        },
        Mode {
            name: "Tight JPEG quality 3, push",
            encoding: Encoding::Tight { quality: 3 },
            requests: Requests::Push,
        },
    ];
    println!(
        "{}x{} panning scene published at {} fps; {} ms one-way delay; {} s measured per row\n",
        options.width,
        options.height,
        options.fps,
        options.delay.as_millis(),
        options.seconds
    );
    for mbps in &options.links_mbps {
        println!("Link: {mbps} Mbit/s");
        println!(
            "  {:<40} {:>10} {:>12} {:>12} {:>10} {:>10}",
            "mode", "delivered", "mean latency", "p95 latency", "Mbit/s", "KB/frame"
        );
        for mode in &modes {
            match run(&options, &scene, mode, *mbps) {
                Ok(outcome) => println!(
                    "  {:<40} {:>6.1} fps {:>9.1} ms {:>9.1} ms {:>10.1} {:>10.1}",
                    mode.name,
                    outcome.delivered_fps,
                    outcome.mean_latency.as_secs_f64() * 1000.0,
                    outcome.p95_latency.as_secs_f64() * 1000.0,
                    outcome.mbit_per_second,
                    outcome.kb_per_frame,
                ),
                Err(error) => println!("  {:<40} failed: {error}", mode.name),
            }
        }
        println!();
    }
    Ok(())
}
