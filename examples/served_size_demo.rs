//! Loopback-only synthetic scene for testing F8 served-size controls.
//! No desktop is captured and remote input is discarded.
use std::{
    error::Error,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use topvnc::{Framebuffer, ServerConfig, VncServer};

fn scene(percent: u8) -> Result<Framebuffer, std::io::Error> {
    let width = (1280 * u32::from(percent) / 100) as u16;
    let height = (720 * u32::from(percent) / 100) as u16;
    let mut frame = Framebuffer::new(width, height)?;
    for y in 0..usize::from(height) {
        for x in 0..usize::from(width) {
            let (nx, ny) = (
                x * 100 / usize::from(percent),
                y * 100 / usize::from(percent),
            );
            frame.pixels_mut()[y * usize::from(width) + x] = if nx % 80 < 2 || ny % 80 < 2 {
                0x00334960
            } else if (520..1160).contains(&nx) && (160..560).contains(&ny) {
                if (nx / 80 + ny / 80) % 2 == 0 {
                    0x002b7180
                } else {
                    0x00205465
                }
            } else {
                0x00121c2b
            };
        }
    }
    Ok(frame)
}

fn main() -> Result<(), Box<dyn Error>> {
    let server = Arc::new(VncServer::bind(
        "127.0.0.1:5998",
        scene(100)?,
        ServerConfig {
            name: "TopVNC served-size demo".into(),
            allow_insecure: true,
            ..ServerConfig::default()
        },
    )?);
    server.set_serve_scale(100);
    let runner = Arc::clone(&server);
    thread::spawn(move || runner.run());
    eprintln!(
        "Synthetic demo on 127.0.0.1:5998 for 5 minutes; explicit unauthenticated loopback only."
    );
    let started = Instant::now();
    let mut applied = 100;
    while started.elapsed() < Duration::from_secs(300) {
        let requested = server.requested_serve_scale();
        if requested != applied {
            server.update_framebuffer(&scene(requested)?)?;
            server.set_serve_scale(requested);
            applied = requested;
            eprintln!("Applied {applied}%");
        }
        while server.try_event().is_ok() {}
        thread::sleep(Duration::from_millis(16));
    }
    server.stop();
    Ok(())
}
