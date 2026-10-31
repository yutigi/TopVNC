use crate::session::*;
use topvnc_e2e::{Channel, Keypair, Responder};
use topvnc_rfb::Encoding;
use topvnc_rfb::messages::{
    PIXEL_FORMAT_MESSAGE, continuous_updates_message, fence_message, key_event, pointer_event,
    set_encodings_message, update_request, vnc_auth_response,
};
use topvnc_rfb::protocol::{
    BUTTON_LEFT, CONTINUOUS_UPDATES_MESSAGE, DESKTOP_SIZE_ENCODING, FENCE_REQUEST,
};

const CHALLENGE: [u8; 16] = [3; 16];

/// The host end: the gateway's Noise responder and a scripted RFB server.
struct Host {
    channel: Channel,
    /// Plaintext the browser sent, not yet checked.
    received: Vec<u8>,
}

fn connect(quality: u8) -> (ClientSession, Host) {
    let host = Keypair::generate().unwrap();
    let browser = Keypair::generate().unwrap();
    let mut session = ClientSession::new(&browser.private, &host.public, None, quality).unwrap();
    let first = session.take_outgoing().unwrap();
    let (responder, reply) = Responder::accept(&host.private, &first, None).unwrap();
    assert_eq!(session.receive(&reply, 0.0).unwrap(), EVENT_SECURE);
    let last = session.take_outgoing().unwrap();
    let (channel, key, _) = responder.finish(&last).unwrap();
    assert_eq!(key, browser.public);
    (
        session,
        Host {
            channel,
            received: Vec::new(),
        },
    )
}

impl Host {
    /// Send `bytes` sealed in chunks of `chunk`, as the gateway's TCP reads
    /// may split them, and collect the events.
    fn send(&mut self, session: &mut ClientSession, bytes: &[u8], chunk: usize, now: f64) -> u32 {
        let mut events = 0;
        for part in bytes.chunks(chunk) {
            events |= session
                .receive(&self.channel.seal(part).unwrap(), now)
                .unwrap();
        }
        self.collect(session);
        events
    }

    fn collect(&mut self, session: &mut ClientSession) {
        while let Some(message) = session.take_outgoing() {
            self.received.extend(self.channel.open(&message).unwrap());
        }
    }

    /// Assert the browser sent exactly `expected` since the last check.
    fn expect(&mut self, session: &mut ClientSession, expected: &[u8]) {
        self.collect(session);
        assert_eq!(self.received, expected);
        self.received.clear();
    }
}

fn server_init(width: u16, height: u16) -> Vec<u8> {
    let mut init = Vec::new();
    init.extend_from_slice(&width.to_be_bytes());
    init.extend_from_slice(&height.to_be_bytes());
    init.extend_from_slice(&[0; 16]);
    init.extend_from_slice(&4u32.to_be_bytes());
    init.extend_from_slice(b"desk");
    init
}

fn rect(x: u16, y: u16, width: u16, height: u16, encoding: i32) -> Vec<u8> {
    let mut rect = Vec::new();
    for value in [x, y, width, height] {
        rect.extend_from_slice(&value.to_be_bytes());
    }
    rect.extend_from_slice(&encoding.to_be_bytes());
    rect
}

fn update(rects: &[Vec<u8>]) -> Vec<u8> {
    let mut message = vec![0, 0];
    message.extend_from_slice(&(rects.len() as u16).to_be_bytes());
    for rect in rects {
        message.extend_from_slice(rect);
    }
    message
}

/// Run the password handshake with the server's bytes cut into `chunk`s and
/// return the session with the browser's requests checked.
fn signed_in(chunk: usize) -> (ClientSession, Host) {
    let (mut session, mut host) = connect(6);
    let mut server = b"RFB 003.008\n".to_vec();
    server.extend_from_slice(&[1, 2]);
    server.extend_from_slice(&CHALLENGE);
    assert_eq!(
        host.send(&mut session, &server, chunk, 0.0) & EVENT_PASSWORD,
        EVENT_PASSWORD
    );
    let mut expected = b"RFB 003.008\n".to_vec();
    expected.push(2);
    host.expect(&mut session, &expected);
    // Input before the session runs is dropped, not queued.
    session.pointer(BUTTON_LEFT, 1, 1).unwrap();
    session.password("secret", 0.0).unwrap();
    host.expect(&mut session, &vnc_auth_response(&CHALLENGE, "secret"));
    let mut server = 0u32.to_be_bytes().to_vec();
    server.extend_from_slice(&server_init(4, 2));
    let events = host.send(&mut session, &server, chunk, 0.0);
    assert_eq!(events & EVENT_CONNECTED, EVENT_CONNECTED);
    let mut expected = vec![1];
    expected.extend_from_slice(&PIXEL_FORMAT_MESSAGE);
    expected.extend_from_slice(&set_encodings_message(
        &Encoding::Tight { quality: 6 }.advertised(),
    ));
    expected.extend_from_slice(&update_request(false, 4, 2));
    host.expect(&mut session, &expected);
    assert_eq!(session.server_name(), Some("desk"));
    (session, host)
}

#[test]
fn signs_in_with_the_vnc_password_inside_the_channel_for_any_chunking() {
    let mut pixels = rect(1, 1, 2, 1, 0);
    pixels.extend_from_slice(&[10, 20, 30, 0, 40, 50, 60, 0]);
    let first = update(&[pixels]);
    for chunk in [1, 2, 3, 7, 64, 4096] {
        let (mut session, mut host) = signed_in(chunk);
        session.take_damage();
        let events = host.send(&mut session, &first, chunk, 0.0);
        assert_eq!(events & EVENT_FRAME, EVENT_FRAME);
        let frame = session.frame().unwrap();
        let start = (4 + 1) * 4;
        assert_eq!(
            &frame.pixels()[start..start + 8],
            &[30, 20, 10, 255, 60, 50, 40, 255]
        );
        assert_eq!(session.take_damage().len(), 1);
        // Without continuous updates, each update asks for the next.
        host.expect(&mut session, &update_request(true, 4, 2));
        assert_eq!(session.frames(), 1);
    }
}

#[test]
fn refuses_none_security_and_tampered_messages() {
    let (mut session, host) = connect(6);
    let mut server = b"RFB 003.008\n".to_vec();
    server.extend_from_slice(&[1, 1]);
    let error = session
        .receive(&{ host }.channel.seal(&server).unwrap(), 0.0)
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    // A failed session stays failed.
    assert!(session.receive(&[0; 40], 0.0).is_err());

    let (mut session, mut host) = connect(6);
    let mut sealed = host.channel.seal(b"RFB 003.008\n").unwrap();
    sealed[3] ^= 1;
    assert!(session.receive(&sealed, 0.0).is_err());
}

#[test]
fn continuous_updates_fences_and_resizes_follow_the_server() {
    let (mut session, mut host) = signed_in(4096);
    // The server announces continuous updates; the browser enables them.
    host.send(&mut session, &[CONTINUOUS_UPDATES_MESSAGE], 1, 0.0);
    host.expect(&mut session, &continuous_updates_message(true, 4, 2));
    assert!(session.continuous_updates());
    // A fence request after an update is answered after that update, and no
    // request is sent per update any more.
    let mut stream = update(&[{
        let mut raw = rect(0, 0, 1, 1, 0);
        raw.extend_from_slice(&[1, 2, 3, 0]);
        raw
    }]);
    stream.extend_from_slice(&fence_message(FENCE_REQUEST | 0b111, b"flow"));
    host.send(&mut session, &stream, 5, 0.0);
    host.expect(&mut session, &fence_message(0b111, b"flow"));
    // A resize: the new size is used for continuous updates and a full update.
    let events = host.send(
        &mut session,
        &update(&[rect(0, 0, 8, 6, DESKTOP_SIZE_ENCODING)]),
        3,
        0.0,
    );
    assert_eq!(events & EVENT_RESIZED, EVENT_RESIZED);
    let mut expected = continuous_updates_message(true, 8, 6).to_vec();
    expected.extend_from_slice(&update_request(false, 8, 6));
    host.expect(&mut session, &expected);
    assert_eq!(session.frame().unwrap().width(), 8);
}

#[test]
fn measures_round_trips_with_fences() {
    let (mut session, mut host) = signed_in(4096);
    session.ping(100.0).unwrap();
    host.collect(&mut session);
    let request = std::mem::take(&mut host.received);
    assert_eq!(request[0], 248);
    let payload = &request[9..];
    // A second ping waits for the first.
    session.ping(105.0).unwrap();
    host.expect(&mut session, &[]);
    let events = host.send(&mut session, &fence_message(0, payload), 64, 130.0);
    assert_eq!(events & EVENT_RTT, EVENT_RTT);
    assert_eq!(session.rtt(), Some(30.0));
    // An unrelated fence reply does not count.
    session.ping(200.0).unwrap();
    host.collect(&mut session);
    host.received.clear();
    assert_eq!(
        host.send(&mut session, &fence_message(0, b"other"), 64, 230.0) & EVENT_RTT,
        0
    );
}

#[test]
fn input_is_encoded_bounded_to_the_framebuffer_and_released() {
    let (mut session, mut host) = signed_in(4096);
    session.pointer(BUTTON_LEFT, 100, 100).unwrap();
    host.expect(
        &mut session,
        &pointer_event(BUTTON_LEFT, 3, 1, false).0[..6],
    );
    assert!(session.key_code("KeyA", true).unwrap());
    assert!(!session.key_code("KeyA", true).unwrap(), "repeat");
    session.key_sym(0xffe3, true).unwrap(); // A latched toolbar Control.
    session.type_text("!").unwrap();
    let mut expected = key_event(0x61, true).to_vec();
    expected.extend_from_slice(&key_event(0xffe3, true));
    for (keysym, down) in [(0xffe1, true), (0x21, true), (0x21, false), (0xffe1, false)] {
        expected.extend_from_slice(&key_event(keysym, down));
    }
    host.expect(&mut session, &expected);
    // Losing focus releases what is still held: Control, A, then the button.
    session.release_all().unwrap();
    let mut expected = key_event(0xffe3, false).to_vec();
    expected.extend_from_slice(&key_event(0x61, false));
    expected.extend_from_slice(&pointer_event(0, 3, 1, false).0[..6]);
    host.expect(&mut session, &expected);
    // The physical key was forgotten too, so its late release sends nothing.
    assert!(!session.key_code("KeyA", false).unwrap());
    // Quality changes re-advertise the encodings.
    session.set_quality(2).unwrap();
    host.expect(
        &mut session,
        &set_encodings_message(&Encoding::Tight { quality: 2 }.advertised()),
    );
}

#[test]
fn large_input_bursts_are_sealed_in_bounded_messages() {
    let (mut session, mut host) = signed_in(4096);
    session.type_text(&"x".repeat(5000)).unwrap();
    let mut messages = 0;
    while let Some(message) = session.take_outgoing() {
        assert!(message.len() <= topvnc_e2e::MAX_CIPHERTEXT_BYTES);
        host.received.extend(host.channel.open(&message).unwrap());
        messages += 1;
    }
    // Text is cut to the per-call limit: two events of eight bytes per
    // character.
    assert_eq!(host.received.len(), crate::keys::MAX_TEXT_CHARS * 16);
    assert!(messages >= 1);
}
