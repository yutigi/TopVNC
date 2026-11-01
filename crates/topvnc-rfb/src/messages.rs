//! Client-to-server messages, encoded without any transport.

use crate::protocol::{
    BUTTON_BACK, BUTTON_FORWARD, CONTINUOUS_UPDATES_MESSAGE, EXTENDED_POINTER_MARKER,
    FENCE_MESSAGE, MAX_FENCE_PAYLOAD,
};
use des::Des;
use des::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};

/// SetPixelFormat: 32-bit little-endian true color, on the wire B, G, R,
/// unused.
pub const PIXEL_FORMAT_MESSAGE: [u8; 20] = [
    0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0,
];

pub fn set_encodings_message(encodings: &[i32]) -> Vec<u8> {
    let mut message = vec![2, 0];
    message.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
    for encoding in encodings {
        message.extend_from_slice(&encoding.to_be_bytes());
    }
    message
}

/// FramebufferUpdateRequest for the whole `width` by `height` framebuffer.
pub fn update_request(incremental: bool, width: u16, height: u16) -> [u8; 10] {
    let mut message = [3, u8::from(incremental), 0, 0, 0, 0, 0, 0, 0, 0];
    message[6..8].copy_from_slice(&width.to_be_bytes());
    message[8..10].copy_from_slice(&height.to_be_bytes());
    message
}

/// EnableContinuousUpdates for the whole framebuffer, or disable them.
pub fn continuous_updates_message(enable: bool, width: u16, height: u16) -> [u8; 10] {
    let mut message = update_request(enable, width, height);
    message[0] = CONTINUOUS_UPDATES_MESSAGE;
    message
}

pub fn fence_message(flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut message = vec![FENCE_MESSAGE, 0, 0, 0];
    message.extend_from_slice(&flags.to_be_bytes());
    message.push(payload.len().min(MAX_FENCE_PAYLOAD) as u8);
    message.extend_from_slice(&payload[..payload.len().min(MAX_FENCE_PAYLOAD)]);
    message
}

pub fn key_event(keysym: u32, down: bool) -> [u8; 8] {
    let mut message = [4, u8::from(down), 0, 0, 0, 0, 0, 0];
    message[4..8].copy_from_slice(&keysym.to_be_bytes());
    message
}

/// A PointerEvent and its length: extended (seven bytes) when the server
/// accepts extended events and back or forward is held. Without extended
/// events, back and forward are not sent, since the client advertised them.
pub fn pointer_event(buttons: u16, x: u16, y: u16, extended: bool) -> ([u8; 7], usize) {
    let mut message = [5, (buttons & 0x7f) as u8, 0, 0, 0, 0, 0];
    message[2..4].copy_from_slice(&x.to_be_bytes());
    message[4..6].copy_from_slice(&y.to_be_bytes());
    if extended && buttons & (BUTTON_BACK | BUTTON_FORWARD) != 0 {
        message[1] |= EXTENDED_POINTER_MARKER;
        message[6] =
            u8::from(buttons & BUTTON_BACK != 0) | (u8::from(buttons & BUTTON_FORWARD != 0) << 1);
        return (message, 7);
    }
    (message, 6)
}

/// The response to a VNC authentication challenge. VNC's DES key uses each
/// of the first eight password bytes with its bits reversed.
pub fn vnc_auth_response(challenge: &[u8; 16], password: &str) -> [u8; 16] {
    let mut key = [0u8; 8];
    for (dest, byte) in key.iter_mut().zip(password.as_bytes().iter().copied()) {
        *dest = byte.reverse_bits();
    }
    let cipher = Des::new_from_slice(&key).expect("DES key is always eight bytes");
    let mut response = *challenge;
    for chunk in response.as_chunks_mut::<8>().0.iter_mut() {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    response
}
