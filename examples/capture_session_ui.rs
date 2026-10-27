//! Capture the real software-rendered F8 panel without a GPU or live desktop.
//! Writes a binary PPM to stdout; convert it to PNG for documentation.
#[allow(dead_code)]
#[path = "../src/desktop_host.rs"]
mod desktop_host;
#[allow(dead_code)]
#[path = "../src/helpers.rs"]
mod helpers;
#[allow(dead_code)]
#[path = "../src/ui.rs"]
mod ui;

use std::io::{self, Write};
fn main() -> io::Result<()> {
    let (width, height) = (448, 712);
    let mut pixels = vec![ui::BG; width * height];
    ui::overlay(
        &mut ui::Canvas::new(&mut pixels, width, height),
        &ui::Config {
            debug: std::env::args().any(|arg| arg == "--debug"),
            ..ui::Config::default()
        },
        true,
        if std::env::args().any(|arg| arg == "--unavailable") {
            None
        } else {
            Some(50)
        },
    );
    let mut output = io::BufWriter::new(io::stdout().lock());
    write!(output, "P6\n{width} {height}\n255\n")?;
    for pixel in pixels {
        output.write_all(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8])?;
    }
    output.flush()
}
