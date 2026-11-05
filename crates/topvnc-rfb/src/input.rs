//! Input state shared by a connection's input sources: pointer mode, held
//! buttons and keys, and the host capabilities input messages depend on.
//! Every method appends the messages to send to `out`; the caller writes
//! them to its transport in order.

use crate::invalid;
use crate::messages::{fence_message, key_event, pointer_event, set_encodings_message};
use crate::protocol::{
    FENCE_REQUEST, MAX_RELATIVE_DELTA, MAX_SERVED_DISPLAYS, POINTER_MOTION_CHANGE_ENCODING,
    RELATIVE_POINTER_ORIGIN, SERVE_DISPLAY_FENCE, SERVE_SCALE_FENCE,
};
use crate::{CaptureStatus, ServedDisplays};
use std::io;

/// Keys tracked for [`InputState::release_all`]; more held at once are sent
/// but not tracked.
const MAX_HELD_KEYS: usize = 64;

pub struct InputState {
    /// The encodings advertised at the handshake.
    encodings: Vec<i32>,
    /// The client advertises relative pointer motion.
    relative_allowed: bool,
    /// The server asked for relative pointer motion.
    relative: bool,
    /// The server acknowledged extended PointerEvents.
    extended_buttons: bool,
    serve_scale: Option<u8>,
    serve_display: Option<ServedDisplays>,
    capture_status: CaptureStatus,
    /// The last absolute position sent.
    position: (u16, u16),
    /// The buttons last sent.
    buttons: u16,
    held_keys: Vec<u32>,
}

impl InputState {
    pub fn new(encodings: Vec<i32>) -> Self {
        Self {
            relative_allowed: encodings.contains(&POINTER_MOTION_CHANGE_ENCODING),
            encodings,
            relative: false,
            extended_buttons: false,
            serve_scale: None,
            serve_display: None,
            capture_status: CaptureStatus::Live,
            position: (0, 0),
            buttons: 0,
            held_keys: Vec::new(),
        }
    }

    fn send_pointer(&mut self, buttons: u16, x: u16, y: u16, out: &mut Vec<u8>) {
        let (message, length) = pointer_event(buttons, x, y, self.extended_buttons);
        self.buttons = buttons;
        out.extend_from_slice(&message[..length]);
    }

    /// Send `buttons` without motion, if they changed.
    fn send_buttons(&mut self, buttons: u16, out: &mut Vec<u8>) {
        if buttons == self.buttons {
            return;
        }
        let (x, y) = if self.relative {
            (
                RELATIVE_POINTER_ORIGIN as u16,
                RELATIVE_POINTER_ORIGIN as u16,
            )
        } else {
            self.position
        };
        self.send_pointer(buttons, x, y, out);
    }

    /// The pointer is at (`x`, `y`) with `buttons` (`BUTTON_*` bits) held.
    /// While the server asks for relative motion, only button changes are
    /// sent.
    pub fn pointer(&mut self, buttons: u16, x: u16, y: u16, out: &mut Vec<u8>) {
        if self.relative {
            return self.send_buttons(buttons, out);
        }
        self.position = (x, y);
        self.send_pointer(buttons, x, y, out);
    }

    /// The pointer moved by (`dx`, `dy`) device units with `buttons` held.
    /// While the server asks for absolute positions, only button changes
    /// are sent, at the last position.
    pub fn pointer_motion(&mut self, buttons: u16, dx: i32, dy: i32, out: &mut Vec<u8>) {
        if !self.relative {
            return self.send_buttons(buttons, out);
        }
        let (mut dx, mut dy) = (dx, dy);
        loop {
            let step_x = dx.clamp(-MAX_RELATIVE_DELTA, MAX_RELATIVE_DELTA);
            let step_y = dy.clamp(-MAX_RELATIVE_DELTA, MAX_RELATIVE_DELTA);
            self.send_pointer(
                buttons,
                (RELATIVE_POINTER_ORIGIN + step_x) as u16,
                (RELATIVE_POINTER_ORIGIN + step_y) as u16,
                out,
            );
            (dx, dy) = (dx - step_x, dy - step_y);
            if (dx, dy) == (0, 0) {
                return;
            }
        }
    }

    pub fn key(&mut self, keysym: u32, down: bool, out: &mut Vec<u8>) {
        let held = self.held_keys.iter().position(|key| *key == keysym);
        match (down, held) {
            (true, None) if self.held_keys.len() < MAX_HELD_KEYS => self.held_keys.push(keysym),
            (false, Some(index)) => {
                self.held_keys.remove(index);
            }
            _ => {}
        }
        out.extend_from_slice(&key_event(keysym, down));
    }

    /// The buttons last sent.
    pub fn buttons(&self) -> u16 {
        self.buttons
    }

    /// Keys pressed and not yet released, oldest first.
    pub fn held_keys(&self) -> &[u32] {
        &self.held_keys
    }

    /// Release every held key and button, as when the viewer loses focus
    /// or a gesture is cancelled, so nothing stays pressed on the host.
    pub fn release_all(&mut self, out: &mut Vec<u8>) {
        while let Some(keysym) = self.held_keys.pop() {
            out.extend_from_slice(&key_event(keysym, false));
        }
        self.send_buttons(0, out);
    }

    /// Whether the server asks for relative motion: the viewer should lock
    /// and hide its pointer and send [`InputState::pointer_motion`].
    pub fn relative_pointer(&self) -> bool {
        self.relative
    }

    /// Advertise relative pointer motion (the default) or stop. Declining
    /// switches to absolute positions at once.
    pub fn set_relative_pointer_allowed(&mut self, allowed: bool, out: &mut Vec<u8>) {
        if self.relative_allowed == allowed {
            return;
        }
        self.relative_allowed = allowed;
        self.relative = false;
        let encodings = self
            .encodings
            .iter()
            .copied()
            .filter(|encoding| allowed || *encoding != POINTER_MOTION_CHANGE_ENCODING)
            .collect::<Vec<_>>();
        out.extend_from_slice(&set_encodings_message(&encodings));
    }

    /// Advertise a new encoding list, for example another JPEG quality,
    /// keeping the relative-pointer choice.
    pub fn advertise(&mut self, encodings: Vec<i32>, out: &mut Vec<u8>) {
        self.encodings = encodings;
        let encodings = self
            .encodings
            .iter()
            .copied()
            .filter(|encoding| self.relative_allowed || *encoding != POINTER_MOTION_CHANGE_ENCODING)
            .collect::<Vec<_>>();
        out.extend_from_slice(&set_encodings_message(&encodings));
    }

    /// The server switched pointer modes. Input sent after this uses the
    /// new mode, so it happens before the following fence is answered.
    pub fn server_pointer_mode(&mut self, relative: bool) {
        // A switch announced before the client declined is ignored.
        self.relative = relative && self.relative_allowed;
    }

    pub fn server_extended_buttons(&mut self) {
        self.extended_buttons = true;
    }

    /// Current host-wide served percentage, when the host offers control.
    pub fn serve_scale(&self) -> Option<u8> {
        self.serve_scale
    }

    pub fn set_serve_scale(&mut self, percent: u8) {
        self.serve_scale = Some(percent);
    }

    pub fn request_serve_scale(&self, percent: u8, out: &mut Vec<u8>) -> io::Result<()> {
        if !(25..=100).contains(&percent) {
            return Err(invalid("served size must be 25..100 percent"));
        }
        if self.serve_scale.is_none() {
            return Err(invalid("server does not offer served-size control"));
        }
        let mut payload = SERVE_SCALE_FENCE.to_vec();
        payload.push(percent);
        out.extend_from_slice(&fence_message(FENCE_REQUEST, &payload));
        Ok(())
    }

    /// The host display served, when the host offers display switching.
    pub fn serve_display(&self) -> Option<ServedDisplays> {
        self.serve_display
    }

    pub fn set_serve_display(&mut self, displays: ServedDisplays) {
        self.serve_display = Some(displays);
    }

    /// Whether the host can capture its desktop; live until it says not.
    pub fn capture_status(&self) -> CaptureStatus {
        self.capture_status
    }

    pub fn set_capture_status(&mut self, status: CaptureStatus) {
        self.capture_status = status;
    }

    /// Ask the host to serve `display`, numbered from 1, or 0 for all
    /// displays. The host's choice is shared by every viewer.
    pub fn request_serve_display(&self, display: u8, out: &mut Vec<u8>) -> io::Result<()> {
        if display > MAX_SERVED_DISPLAYS {
            return Err(invalid("display number is out of range"));
        }
        if self.serve_display.is_none() {
            return Err(invalid("server does not offer display switching"));
        }
        let mut payload = SERVE_DISPLAY_FENCE.to_vec();
        payload.push(display);
        out.extend_from_slice(&fence_message(FENCE_REQUEST, &payload));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{BUTTON_LEFT, BUTTON_RIGHT, EXTENDED_MOUSE_BUTTONS_ENCODING};

    fn state() -> InputState {
        InputState::new(vec![
            0,
            POINTER_MOTION_CHANGE_ENCODING,
            EXTENDED_MOUSE_BUTTONS_ENCODING,
        ])
    }

    #[test]
    fn release_all_lifts_held_keys_newest_first_then_buttons() {
        let mut input = state();
        let mut out = Vec::new();
        input.pointer(BUTTON_LEFT | BUTTON_RIGHT, 10, 20, &mut out);
        input.key(0xffe3, true, &mut out); // Control_L
        input.key(0x61, true, &mut out);
        input.key(0x61, true, &mut out); // Auto-repeat keeps one entry.
        input.key(0x62, true, &mut out);
        input.key(0x62, false, &mut out);
        assert_eq!(input.held_keys(), [0xffe3, 0x61]);
        out.clear();
        input.release_all(&mut out);
        let mut expected = Vec::new();
        expected.extend_from_slice(&key_event(0x61, false));
        expected.extend_from_slice(&key_event(0xffe3, false));
        expected.extend_from_slice(&pointer_event(0, 10, 20, false).0[..6]);
        assert_eq!(out, expected);
        assert!(input.held_keys().is_empty());
        assert_eq!(input.buttons(), 0);
        // Nothing is held any more, so a second release sends nothing.
        out.clear();
        input.release_all(&mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn held_key_tracking_is_bounded() {
        let mut input = state();
        let mut out = Vec::new();
        for keysym in 0..(MAX_HELD_KEYS as u32 + 10) {
            input.key(keysym, true, &mut out);
        }
        assert_eq!(input.held_keys().len(), MAX_HELD_KEYS);
        // Every press is still sent.
        assert_eq!(out.len(), (MAX_HELD_KEYS + 10) * 8);
    }

    #[test]
    fn relative_motion_is_split_and_buttons_follow_the_mode() {
        let mut input = state();
        let mut out = Vec::new();
        input.server_pointer_mode(true);
        input.pointer_motion(BUTTON_LEFT, 2 * MAX_RELATIVE_DELTA + 1, 0, &mut out);
        assert_eq!(out.len(), 3 * 6);
        // Absolute positions are ignored in relative mode except for buttons.
        out.clear();
        input.pointer(0, 5, 5, &mut out);
        let origin = RELATIVE_POINTER_ORIGIN as u16;
        assert_eq!(out, pointer_event(0, origin, origin, false).0[..6]);
        // Declining relative motion switches back and re-advertises.
        out.clear();
        input.set_relative_pointer_allowed(false, &mut out);
        assert!(!input.relative_pointer());
        assert_eq!(
            out,
            set_encodings_message(&[0, EXTENDED_MOUSE_BUTTONS_ENCODING])
        );
    }
}
