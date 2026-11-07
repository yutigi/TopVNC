//! The pairing invite as the WEB tab shows it: a QR code drawn in TopVNC's
//! own window. The link stays in memory for the explicit clipboard action.

use std::time::Instant;

/// A QR code for one pairing invite.
#[derive(Clone)]
pub struct PairingCode {
    link: String,
    /// Modules per side, without the quiet zone.
    pub width: usize,
    /// Row-major, `true` for dark modules.
    pub modules: Vec<bool>,
    pub expires: Instant,
}

impl PairingCode {
    /// Encode `link` and retain it only for this invite's lifetime.
    pub fn new(link: String, expires: Instant) -> Result<Self, String> {
        let code = qrcode::QrCode::new(link.as_bytes())
            .map_err(|error| format!("could not make the pairing QR code: {error}"))?;
        Ok(Self {
            link,
            width: code.width(),
            modules: code
                .to_colors()
                .into_iter()
                .map(|color| color == qrcode::Color::Dark)
                .collect(),
            expires,
        })
    }

    /// An expired invite must not be copied, even before the next runtime poll.
    pub fn copy_url(&self, now: Instant) -> Option<&str> {
        (now < self.expires).then_some(self.link.as_str())
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

impl std::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Both the link and the QR modules encode the pairing secret.
        f.debug_struct("PairingCode")
            .field("width", &self.width)
            .field("expires", &self.expires)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn copying_expires_and_debug_does_not_reveal_the_invite() {
        let now = Instant::now();
        let link = "https://test.trycloudflare.com/#pair=private-invite";
        let code = PairingCode::new(link.into(), now + Duration::from_secs(600)).unwrap();
        assert_eq!(code.copy_url(now), Some(link));
        assert_eq!(code.copy_url(now + Duration::from_secs(600)), None);
        let debug = format!("{code:?}");
        assert!(!debug.contains("private-invite"));
        assert!(!debug.contains("modules"));
    }

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
