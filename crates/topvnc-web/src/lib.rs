//! TopVNC's browser client, compiled to WebAssembly. The page owns the
//! WebSocket, canvas, and DOM events; this crate owns everything that touches
//! protocol bytes: the browser-to-host Noise channel, the shared `topvnc-rfb`
//! handshake and decoders, the framebuffer, and input encoding. The VNC
//! password is used for one handshake and never stored.
//!
//! Keys cross the boundary as unpadded base64url strings, messages as bytes.

pub mod frame;
pub mod keys;
pub mod session;

use session::ClientSession;
use topvnc_e2e::{self as e2e, Keypair, PairingInvite};
use wasm_bindgen::prelude::*;

type JsResult<T> = Result<T, String>;

fn error(message: impl std::fmt::Display) -> String {
    message.to_string()
}

/// Generates a browser private key.
#[wasm_bindgen(js_name = newBrowserKey)]
pub fn new_browser_key() -> JsResult<String> {
    Ok(e2e::encode_key(
        &Keypair::generate().map_err(error)?.private,
    ))
}

#[wasm_bindgen(js_name = publicKey)]
pub fn public_key(private: &str) -> JsResult<String> {
    let keypair = Keypair::from_private(e2e::decode_key(private).map_err(error)?).map_err(error)?;
    Ok(e2e::encode_key(&keypair.public))
}

#[wasm_bindgen]
pub fn fingerprint(public: &str) -> JsResult<String> {
    Ok(e2e::fingerprint(&e2e::decode_key(public).map_err(error)?))
}

#[wasm_bindgen]
pub struct Invite {
    host: String,
    secret: String,
}

#[wasm_bindgen]
impl Invite {
    #[wasm_bindgen(getter)]
    pub fn host(&self) -> String {
        self.host.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn secret(&self) -> String {
        self.secret.clone()
    }
}

#[wasm_bindgen(js_name = parseInvite)]
pub fn parse_invite(text: &str) -> JsResult<Invite> {
    let invite = PairingInvite::decode(text).map_err(error)?;
    Ok(Invite {
        host: e2e::encode_key(&invite.host),
        secret: e2e::encode_key(&invite.secret),
    })
}

/// One connection to the host. Any error leaves it failed; open a new one.
#[wasm_bindgen]
pub struct Connection {
    inner: ClientSession,
}

#[wasm_bindgen]
impl Connection {
    /// Pins `host_public`. Passing `pairing_secret` enrolls this browser key.
    #[wasm_bindgen(constructor)]
    pub fn new(
        browser_private: &str,
        host_public: &str,
        pairing_secret: Option<String>,
        quality: u8,
    ) -> JsResult<Connection> {
        let browser = e2e::decode_key(browser_private).map_err(error)?;
        let host = e2e::decode_key(host_public).map_err(error)?;
        let secret = pairing_secret
            .as_deref()
            .map(e2e::decode_key)
            .transpose()
            .map_err(error)?;
        Ok(Self {
            inner: ClientSession::new(&browser, &host, secret.as_ref(), quality).map_err(error)?,
        })
    }

    /// The next message to send, if any. Send them all, in order.
    #[wasm_bindgen(js_name = takeOutgoing)]
    pub fn take_outgoing(&mut self) -> Option<Vec<u8>> {
        self.inner.take_outgoing()
    }

    /// Handle one WebSocket message; returns `EVENT_*` bits.
    pub fn receive(&mut self, message: &[u8], now: f64) -> JsResult<u32> {
        self.inner.receive(message, now).map_err(error)
    }

    pub fn password(&mut self, password: &str, now: f64) -> JsResult<u32> {
        self.inner.password(password, now).map_err(error)
    }

    pub fn width(&self) -> u16 {
        self.inner.frame().map_or(0, |frame| frame.width())
    }

    pub fn height(&self) -> u16 {
        self.inner.frame().map_or(0, |frame| frame.height())
    }

    /// Where the RGBA framebuffer starts in WebAssembly memory. Views of it
    /// are invalidated when memory grows or the framebuffer is resized.
    #[wasm_bindgen(js_name = framePointer)]
    pub fn frame_pointer(&self) -> usize {
        self.inner
            .frame()
            .map_or(0, |frame| frame.pixels().as_ptr() as usize)
    }

    #[wasm_bindgen(js_name = frameLength)]
    pub fn frame_length(&self) -> usize {
        self.inner.frame().map_or(0, |frame| frame.pixels().len())
    }

    /// Regions changed since the last call, as x, y, width, height values.
    #[wasm_bindgen(js_name = takeDamage)]
    pub fn take_damage(&mut self) -> Vec<u16> {
        self.inner
            .take_damage()
            .iter()
            .flat_map(|rect| [rect.x, rect.y, rect.width, rect.height])
            .collect()
    }

    pub fn pointer(&mut self, buttons: u16, x: u16, y: u16) -> JsResult<()> {
        self.inner.pointer(buttons, x, y).map_err(error)
    }

    #[wasm_bindgen(js_name = pointerMotion)]
    pub fn pointer_motion(&mut self, buttons: u16, dx: i32, dy: i32) -> JsResult<()> {
        self.inner.pointer_motion(buttons, dx, dy).map_err(error)
    }

    /// A physical key by `KeyboardEvent.code`; returns whether it was sent.
    #[wasm_bindgen(js_name = keyCode)]
    pub fn key_code(&mut self, code: &str, down: bool) -> JsResult<bool> {
        self.inner.key_code(code, down).map_err(error)
    }

    #[wasm_bindgen(js_name = keySym)]
    pub fn key_sym(&mut self, keysym: u32, down: bool) -> JsResult<()> {
        self.inner.key_sym(keysym, down).map_err(error)
    }

    #[wasm_bindgen(js_name = typeText)]
    pub fn type_text(&mut self, text: &str) -> JsResult<()> {
        self.inner.type_text(text).map_err(error)
    }

    #[wasm_bindgen(js_name = releaseAll)]
    pub fn release_all(&mut self) -> JsResult<()> {
        self.inner.release_all().map_err(error)
    }

    #[wasm_bindgen(js_name = setQuality)]
    pub fn set_quality(&mut self, quality: u8) -> JsResult<()> {
        self.inner.set_quality(quality).map_err(error)
    }

    /// The host's served percentage, or 0 when it offers no control.
    #[wasm_bindgen(js_name = serveScale)]
    pub fn serve_scale(&self) -> u8 {
        self.inner.serve_scale().unwrap_or(0)
    }

    #[wasm_bindgen(js_name = requestServeScale)]
    pub fn request_serve_scale(&mut self, percent: u8) -> JsResult<()> {
        self.inner.request_serve_scale(percent).map_err(error)
    }

    /// The display served (0 for all) and the display count, or empty.
    #[wasm_bindgen(js_name = servedDisplays)]
    pub fn served_displays(&self) -> Vec<u8> {
        self.inner
            .served_displays()
            .map(|displays| vec![displays.served, displays.count])
            .unwrap_or_default()
    }

    #[wasm_bindgen(js_name = requestServeDisplay)]
    pub fn request_serve_display(&mut self, display: u8) -> JsResult<()> {
        self.inner.request_serve_display(display).map_err(error)
    }

    #[wasm_bindgen(js_name = relativePointer)]
    pub fn relative_pointer(&self) -> bool {
        self.inner.relative_pointer()
    }

    pub fn ping(&mut self, now: f64) -> JsResult<()> {
        self.inner.ping(now).map_err(error)
    }

    /// The last measured round trip through the host in milliseconds, or -1.
    #[wasm_bindgen(js_name = rttMs)]
    pub fn rtt_ms(&self) -> f64 {
        self.inner.rtt().unwrap_or(-1.0)
    }

    #[wasm_bindgen(js_name = receivedBytes)]
    pub fn received_bytes(&self) -> f64 {
        self.inner.received_bytes() as f64
    }

    pub fn frames(&self) -> f64 {
        self.inner.frames() as f64
    }

    #[wasm_bindgen(js_name = continuousUpdates)]
    pub fn continuous_updates(&self) -> bool {
        self.inner.continuous_updates()
    }

    #[wasm_bindgen(js_name = serverName)]
    pub fn server_name(&self) -> String {
        self.inner.server_name().unwrap_or_default().to_owned()
    }
}

#[cfg(test)]
mod tests;
