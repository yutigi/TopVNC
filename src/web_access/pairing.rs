//! The pairing invite as the WEB tab shows it: a QR code drawn in TopVNC's
//! own window. The link is consumed when the code is made and is not kept.

use std::time::Instant;

/// A QR code for one pairing invite.
#[derive(Clone, Debug)]
pub struct PairingCode {
    /// Modules per side, without the quiet zone.
    pub width: usize,
    /// Row-major, `true` for dark modules.
    pub modules: Vec<bool>,
    pub expires: Instant,
}

impl PairingCode {
    /// Encode `link`, which is dropped here.
    pub fn new(link: String, expires: Instant) -> Result<Self, String> {
        let code = qrcode::QrCode::new(link.as_bytes())
            .map_err(|error| format!("could not make the pairing QR code: {error}"))?;
        drop(link);
        Ok(Self {
            width: code.width(),
            modules: code
                .to_colors()
                .into_iter()
                .map(|color| color == qrcode::Color::Dark)
                .collect(),
            expires,
        })
    }

    pub fn dark(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.width && self.modules[y * self.width + x]
    }

    /// Whole minutes left, rounded up, as the countdown shows them.
    pub fn minutes_left(&self, now: Instant) -> u64 {
        self.expires
            .saturating_duration_since(now)
            .as_secs()
            .div_ceil(60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_typical_invite_fits_a_small_code() {
        // The origin plus an encoded invite of about the gateway's length.
        let link = format!(
            "https://topvnc.subdomain.workers.dev/#pair={}",
            "A".repeat(110)
        );
        let now = Instant::now();
        let code = PairingCode::new(link, now + Duration::from_secs(600)).unwrap();
        assert_eq!(code.modules.len(), code.width * code.width);
        assert!(code.width <= 57, "QR version too large: {}", code.width);
        // Finder pattern corner.
        assert!(code.dark(0, 0) && code.dark(6, 6) && !code.dark(1, 1));
        assert!(!code.dark(code.width, 0));
        // Same orientation as the library's own rendering, which the gateway
        // console prints and phones have scanned.
        let reference = qrcode::QrCode::new(
            format!(
                "https://topvnc.subdomain.workers.dev/#pair={}",
                "A".repeat(110)
            )
            .as_bytes(),
        )
        .unwrap()
        .render::<char>()
        .quiet_zone(false)
        .module_dimensions(1, 1)
        .dark_color('#')
        .light_color('.')
        .build();
        for (y, row) in reference.lines().enumerate() {
            for (x, module) in row.chars().enumerate() {
                assert_eq!(code.dark(x, y), module == '#', "module {x},{y}");
            }
        }
        assert_eq!(reference.lines().count(), code.width);
        assert_eq!(code.minutes_left(now), 10);
        assert_eq!(code.minutes_left(now + Duration::from_secs(541)), 1);
        assert_eq!(code.minutes_left(now + Duration::from_secs(700)), 0);
    }
}
