use super::*;

struct Parties {
    host: Keypair,
    browser: Keypair,
}

fn parties() -> Parties {
    Parties {
        host: Keypair::generate().unwrap(),
        browser: Keypair::generate().unwrap(),
    }
}

fn connect(p: &Parties, secret: Option<&Key>, host_secret: Option<&Key>) -> (Channel, Channel) {
    let (initiator, first) = Initiator::start(&p.browser.private, &p.host.public, secret).unwrap();
    assert_eq!(first.len(), FIRST_MESSAGE_BYTES);
    let (responder, reply) = Responder::accept(&p.host.private, &first, host_secret).unwrap();
    let (last, browser) = initiator.finish(&reply).unwrap();
    let (host, key, mode) = responder.finish(&last).unwrap();
    assert_eq!(key, p.browser.public);
    assert_eq!(
        mode,
        if secret.is_some() {
            Mode::Pairing
        } else {
            Mode::Session
        }
    );
    (browser, host)
}

fn flip(message: &[u8], index: usize) -> Vec<u8> {
    let mut altered = message.to_vec();
    altered[index] ^= 1;
    altered
}

#[test]
fn session_and_pairing_channels_carry_ciphertext_both_ways() {
    let p = parties();
    let invite = PairingInvite::generate(p.host.public).unwrap();
    for secret in [None, Some(&invite.secret)] {
        let (mut browser, mut host) = connect(&p, secret, secret);
        let sealed = browser.seal(b"RFB client bytes").unwrap();
        assert_eq!(sealed.len(), 16 + TAG_BYTES);
        assert!(!sealed.windows(3).any(|w| w == b"RFB"));
        assert_eq!(host.open(&sealed).unwrap(), b"RFB client bytes");
        let sealed = host.seal(b"RFB 003.008\n").unwrap();
        assert_eq!(browser.open(&sealed).unwrap(), b"RFB 003.008\n");
    }
}

#[test]
fn every_session_uses_fresh_keys() {
    let p = parties();
    let (mut first, _) = connect(&p, None, None);
    let (mut second, _) = connect(&p, None, None);
    assert_ne!(first.seal(b"same").unwrap(), second.seal(b"same").unwrap());
}

#[test]
fn host_rejects_a_browser_that_pinned_another_host_key() {
    let p = parties();
    let other = Keypair::generate().unwrap();
    let (_, first) = Initiator::start(&p.browser.private, &other.public, None).unwrap();
    assert_eq!(
        Responder::accept(&p.host.private, &first, None).err(),
        Some("E2E handshake failed")
    );
}

#[test]
fn browser_rejects_a_substituted_or_altered_host_reply() {
    let p = parties();
    let (initiator, first) = Initiator::start(&p.browser.private, &p.host.public, None).unwrap();
    let (_, reply) = Responder::accept(&p.host.private, &first, None).unwrap();
    assert_eq!(
        initiator.finish(&flip(&reply, 40)).err(),
        Some("host identity verification failed")
    );
    // A reply produced for another browser's handshake is not accepted either.
    let (initiator, _) = Initiator::start(&p.browser.private, &p.host.public, None).unwrap();
    assert!(initiator.finish(&reply).is_err());
}

#[test]
fn pairing_requires_the_active_invite_secret() {
    let p = parties();
    let invite = PairingInvite::generate(p.host.public).unwrap();
    let other = PairingInvite::generate(p.host.public).unwrap();
    let (_, first) =
        Initiator::start(&p.browser.private, &p.host.public, Some(&invite.secret)).unwrap();
    assert_eq!(
        Responder::accept(&p.host.private, &first, None).err(),
        Some("no pairing invite is active")
    );
    // psk3 is mixed in last, so a wrong secret fails before the host learns a key.
    let (initiator, first) =
        Initiator::start(&p.browser.private, &p.host.public, Some(&invite.secret)).unwrap();
    let (responder, reply) =
        Responder::accept(&p.host.private, &first, Some(&other.secret)).unwrap();
    let (last, _) = initiator.finish(&reply).unwrap();
    assert_eq!(responder.finish(&last).err(), Some("E2E handshake failed"));
}

#[test]
fn rejects_altered_headers_and_handshake_messages() {
    let p = parties();
    let invite = PairingInvite::generate(p.host.public).unwrap();
    let (_, first) = Initiator::start(&p.browser.private, &p.host.public, None).unwrap();
    // The header is bound into the transcript: changing the mode is detected.
    for altered in [flip(&first, 0), {
        let mut m = first.clone();
        m[1] = Mode::Pairing as u8;
        m
    }] {
        assert!(Responder::accept(&p.host.private, &altered, Some(&invite.secret)).is_err());
    }
    for index in [2, 33, FIRST_MESSAGE_BYTES - 1] {
        assert!(Responder::accept(&p.host.private, &flip(&first, index), None).is_err());
    }
    for length in [0, FIRST_MESSAGE_BYTES - 1, FIRST_MESSAGE_BYTES + 1] {
        let mut sized = first.clone();
        sized.resize(length, 0);
        assert!(Responder::accept(&p.host.private, &sized, None).is_err());
    }

    let (initiator, first) = Initiator::start(&p.browser.private, &p.host.public, None).unwrap();
    let (responder, reply) = Responder::accept(&p.host.private, &first, None).unwrap();
    let (last, _) = initiator.finish(&reply).unwrap();
    assert!(responder.finish(&flip(&last, 10)).is_err());
    let (initiator, first) = Initiator::start(&p.browser.private, &p.host.public, None).unwrap();
    let (responder, reply) = Responder::accept(&p.host.private, &first, None).unwrap();
    let (last, _) = initiator.finish(&reply).unwrap();
    assert!(responder.finish(&last[..THIRD_MESSAGE_BYTES - 1]).is_err());
}

/// Chooses which sealed messages reach the receiver, and in what form.
type Delivery = fn(&[Vec<u8>]) -> Vec<Vec<u8>>;

#[test]
fn transport_rejects_replay_reordering_loss_and_tampering_then_stays_closed() {
    let p = parties();
    let cases: [Delivery; 5] = [
        |m| vec![m[0].clone(), m[0].clone()],
        |m| vec![m[1].clone()],
        |m| vec![m[0].clone(), m[2].clone()],
        |m| vec![flip(&m[0], 3)],
        |m| vec![m[0][..m[0].len() - 1].to_vec()],
    ];
    for case in cases {
        let (mut browser, mut host) = connect(&p, None, None);
        let sent: Vec<_> = (0..3u8).map(|n| browser.seal(&[n; 8]).unwrap()).collect();
        let delivered = case(&sent);
        let (last, prefix) = delivered.split_last().unwrap();
        for message in prefix {
            host.open(message).unwrap();
        }
        assert!(host.open(last).is_err());
        assert_eq!(host.open(&sent[0]).err(), Some("E2E channel closed"));
        assert_eq!(host.seal(b"x").err(), Some("E2E channel closed"));
    }
}

#[test]
fn bounds_transport_message_sizes() {
    let p = parties();
    let (mut browser, mut host) = connect(&p, None, None);
    let largest = browser.seal(&vec![7; MAX_PLAINTEXT_BYTES]).unwrap();
    assert_eq!(largest.len(), MAX_CIPHERTEXT_BYTES);
    assert_eq!(host.open(&largest).unwrap().len(), MAX_PLAINTEXT_BYTES);
    assert!(browser.seal(&[]).is_err());
    let (mut browser, _) = connect(&p, None, None);
    assert!(browser.seal(&vec![0; MAX_PLAINTEXT_BYTES + 1]).is_err());
    for length in [0, TAG_BYTES, MAX_CIPHERTEXT_BYTES + 1] {
        let (_, mut host) = connect(&p, None, None);
        assert!(host.open(&vec![0; length]).is_err());
    }
}

#[test]
fn invites_and_keys_round_trip_and_reject_malformed_text() {
    let host = Keypair::generate().unwrap();
    let invite = PairingInvite::generate(host.public).unwrap();
    let text = invite.encode();
    assert!(text.len() <= MAX_INVITE_CHARS);
    assert_eq!(PairingInvite::decode(&text).unwrap(), invite);
    let key = encode_key(&host.public);
    for bad in [
        String::new(),
        format!("v2.{key}.{key}"),
        format!("v1.{key}"),
        format!("v1.{key}.{key}.{key}"),
        format!("v1.{key}.{}", &key[1..]),
        format!("v1.{key}.{key}A"),
        format!("v1.{key}.{}", key.replace(|_| true, "+")),
        format!("v1.{key}.{key}{}", " ".repeat(200)),
    ] {
        assert!(PairingInvite::decode(&bad).is_err(), "{bad}");
    }
    assert_eq!(decode_key(&key).unwrap(), host.public);
    assert_eq!(
        Keypair::from_private(host.private).unwrap().public,
        host.public
    );
}

#[test]
fn fingerprints_are_short_stable_and_key_specific() {
    let a = Keypair::generate().unwrap().public;
    let b = Keypair::generate().unwrap().public;
    let print = fingerprint(&a);
    assert_eq!(print.len(), 19);
    assert_eq!(print.matches('-').count(), 3);
    assert_eq!(print, fingerprint(&a));
    assert_ne!(print, fingerprint(&b));
}
