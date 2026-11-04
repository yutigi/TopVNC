//! WebAssembly bindings for the browser end of the Phase 0 E2E channel.
//! Keys cross the boundary as unpadded base64url strings; messages as bytes.

use topvnc_spike_e2e::{self as e2e, Channel, Initiator, Keypair, PairingInvite};
use wasm_bindgen::prelude::*;

type JsResult<T> = Result<T, String>;

fn error(message: &str) -> String {
    message.into()
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

enum State {
    Handshake(Box<Initiator>),
    Ready(Channel),
    Failed,
}

/// One encrypted session. Any error leaves it permanently failed.
#[wasm_bindgen]
pub struct E2eSession {
    first: Vec<u8>,
    state: State,
}

#[wasm_bindgen]
impl E2eSession {
    /// Pins `host_public`. Passing `pairing_secret` enrolls this browser key.
    #[wasm_bindgen(constructor)]
    pub fn new(
        browser_private: &str,
        host_public: &str,
        pairing_secret: Option<String>,
    ) -> JsResult<E2eSession> {
        let private = e2e::decode_key(browser_private).map_err(error)?;
        let host = e2e::decode_key(host_public).map_err(error)?;
        let secret = pairing_secret
            .as_deref()
            .map(e2e::decode_key)
            .transpose()
            .map_err(error)?;
        let (initiator, first) =
            Initiator::start(&private, &host, secret.as_ref()).map_err(error)?;
        Ok(Self {
            first,
            state: State::Handshake(Box::new(initiator)),
        })
    }

    #[wasm_bindgen(js_name = firstMessage)]
    pub fn first_message(&self) -> Vec<u8> {
        self.first.clone()
    }

    /// Verifies the host reply and returns the final handshake message.
    pub fn finish(&mut self, reply: &[u8]) -> JsResult<Vec<u8>> {
        let State::Handshake(initiator) = std::mem::replace(&mut self.state, State::Failed) else {
            return Err(error("handshake is not in progress"));
        };
        let (last, channel) = initiator.finish(reply).map_err(error)?;
        self.state = State::Ready(channel);
        Ok(last)
    }

    pub fn seal(&mut self, plaintext: &[u8]) -> JsResult<Vec<u8>> {
        self.channel()?.seal(plaintext).map_err(|e| self.fail(e))
    }

    pub fn open(&mut self, message: &[u8]) -> JsResult<Vec<u8>> {
        self.channel()?.open(message).map_err(|e| self.fail(e))
    }

    fn channel(&mut self) -> JsResult<&mut Channel> {
        match &mut self.state {
            State::Ready(channel) => Ok(channel),
            _ => Err(error("E2E channel is not ready")),
        }
    }

    fn fail(&mut self, message: &str) -> String {
        self.state = State::Failed;
        error(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use e2e::Responder;

    #[test]
    fn pairs_then_reconnects_through_the_exported_api() {
        let host = Keypair::generate().unwrap();
        let invite = PairingInvite::generate(host.public).unwrap();
        let parsed = parse_invite(&invite.encode()).unwrap();
        let browser = new_browser_key().unwrap();
        let browser_public = e2e::decode_key(&public_key(&browser).unwrap()).unwrap();
        for (secret, host_secret) in [(Some(parsed.secret()), Some(&invite.secret)), (None, None)] {
            let mut session = E2eSession::new(&browser, &parsed.host(), secret).unwrap();
            let (responder, reply) =
                Responder::accept(&host.private, &session.first_message(), host_secret).unwrap();
            let last = session.finish(&reply).unwrap();
            let (mut channel, key, _) = responder.finish(&last).unwrap();
            assert_eq!(key, browser_public);
            assert_eq!(
                channel.open(&session.seal(b"ping").unwrap()).unwrap(),
                b"ping"
            );
            assert_eq!(
                session.open(&channel.seal(b"pong").unwrap()).unwrap(),
                b"pong"
            );
        }
    }

    #[test]
    fn rejects_malformed_inputs_and_stays_failed() {
        let host = Keypair::generate().unwrap();
        let host_public = e2e::encode_key(&host.public);
        let browser = new_browser_key().unwrap();
        assert!(E2eSession::new("short", &host_public, None).is_err());
        assert!(E2eSession::new(&browser, &host_public, Some("bad".into())).is_err());
        assert!(parse_invite("v1.bad").is_err());
        assert!(fingerprint("bad").is_err());

        let mut session = E2eSession::new(&browser, &host_public, None).unwrap();
        assert!(session.seal(b"early").is_err());
        assert!(session.finish(&[0; e2e::SECOND_MESSAGE_BYTES]).is_err());
        let (_, reply) = Responder::accept(&host.private, &session.first_message(), None).unwrap();
        assert!(session.finish(&reply).is_err());

        let mut session = E2eSession::new(&browser, &host_public, None).unwrap();
        let (_, reply) = Responder::accept(&host.private, &session.first_message(), None).unwrap();
        session.finish(&reply).unwrap();
        assert!(session.open(&[0; 40]).is_err());
        assert!(session.seal(b"after failure").is_err());
    }
}
