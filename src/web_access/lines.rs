//! Rules for the output lines of the supervised programs. The gateway's
//! stdout protocol is TopVNC's own (`--supervised`); cloudflared's log
//! wording is not a stable interface and was taken from cloudflared
//! 2026.10.3.

/// Something TopVNC acts on, found in a child's output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The gateway's host key fingerprint.
    Fingerprint(String),
    /// The gateway is listening at this address.
    GatewayReady(String),
    /// The Quick Tunnel gateway owns this listener, awaiting its origin.
    GatewayBound(String),
    /// A one-time pairing invite. The link is a secret for the QR code only.
    Pair {
        seconds: u64,
        link: String,
    },
    PairError(String),
    /// A browser with this key fingerprint enrolled.
    Enrolled(String),
    /// cloudflared registered tunnel connection `connIndex`.
    Registered(u32),
    /// cloudflared lost connection `connIndex`, or every connection.
    Lost(Option<u32>),
    /// cloudflared created a Quick Tunnel at this exact
    /// `https://<label>.trycloudflare.com` origin.
    QuickTunnelUrl(String),
}

/// Which pipe a line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Which program a child is; it selects the line rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    Tunnel,
    Gateway,
}

pub fn classify(program: Program, stream: Stream, line: &str) -> Option<Event> {
    match (program, stream) {
        (Program::Gateway, Stream::Stdout) => gateway_stdout(line),
        (Program::Gateway, Stream::Stderr) => line
            .trim()
            .strip_prefix("enrolled browser key ")
            .map(|fingerprint| Event::Enrolled(fingerprint.trim().into())),
        // cloudflared logs to stderr; both pipes are read the same way.
        (Program::Tunnel, _) => tunnel_line(line),
    }
}

fn gateway_stdout(line: &str) -> Option<Event> {
    let line = line.trim();
    let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
    match word {
        "fingerprint" if !rest.is_empty() => Some(Event::Fingerprint(rest.into())),
        "ready" if !rest.is_empty() => Some(Event::GatewayReady(rest.into())),
        "bound" if !rest.is_empty() => Some(Event::GatewayBound(rest.into())),
        "pair" => {
            let (seconds, link) = rest.split_once(' ')?;
            let seconds = seconds.parse().ok().filter(|s| (1..=3600).contains(s))?;
            (link.starts_with("https://") && link.contains("#pair=")).then(|| Event::Pair {
                seconds,
                link: link.into(),
            })
        }
        "pair-error" => Some(Event::PairError(rest.into())),
        _ => None,
    }
}

fn conn_index(line: &str) -> Option<u32> {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix("connIndex="))
        .and_then(|value| value.parse().ok())
}

/// The Quick Tunnel URL inside cloudflared's banner box. Other links in its
/// output (terms of use, documentation) are not a single-label
/// `trycloudflare.com` origin and fail the gateway's own origin rule.
fn quick_tunnel_url(line: &str) -> Option<String> {
    if !line.contains(topvnc_access::QUICK_TUNNEL_SUFFIX) {
        return None;
    }
    line.split(|c: char| c.is_whitespace() || c == '|')
        .find(|word| topvnc_access::validate_quick_tunnel_origin(word).is_ok())
        .map(str::to_owned)
}

fn tunnel_line(line: &str) -> Option<Event> {
    if let Some(url) = quick_tunnel_url(line) {
        Some(Event::QuickTunnelUrl(url))
    } else if line.contains("Unregistered tunnel connection")
        || line.contains("Connection terminated")
    {
        Some(Event::Lost(conn_index(line)))
    } else if line.contains("Registered tunnel connection") {
        conn_index(line).map(Event::Registered)
    } else if line.contains("Lost connection with the edge") {
        Some(Event::Lost(conn_index(line)))
    } else {
        None
    }
}

/// The marker of a pairing link; the secret follows it.
const PAIR_MARKER: &str = "#pair=";

/// The line as it may be kept and shown: control characters become spaces,
/// a pairing secret is removed, and the length is bounded.
pub fn sanitize(line: &str, max_chars: usize) -> String {
    // Remove every secret first, so the length cut never keeps part of one.
    let mut hidden = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(PAIR_MARKER) {
        hidden.push_str(&rest[..at]);
        hidden.push_str("#pair=[hidden]");
        rest = &rest[at + PAIR_MARKER.len()..];
        rest = rest
            .find(char::is_whitespace)
            .map_or("", |end| &rest[end..]);
    }
    hidden.push_str(rest);
    hidden
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_protocol_lines() {
        let g = |line| classify(Program::Gateway, Stream::Stdout, line);
        assert_eq!(
            g("fingerprint a1d1-0000-1111-2222"),
            Some(Event::Fingerprint("a1d1-0000-1111-2222".into()))
        );
        assert_eq!(
            g("ready 127.0.0.1:6080\r"),
            Some(Event::GatewayReady("127.0.0.1:6080".into()))
        );
        assert_eq!(
            g("pair 600 https://topvnc.x.workers.dev/#pair=SECRET"),
            Some(Event::Pair {
                seconds: 600,
                link: "https://topvnc.x.workers.dev/#pair=SECRET".into()
            })
        );
        assert_eq!(
            g("pair-error browser enrollment list is full"),
            Some(Event::PairError("browser enrollment list is full".into()))
        );
        assert_eq!(
            g("bound 127.0.0.1:6080"),
            Some(Event::GatewayBound("127.0.0.1:6080".into()))
        );
        for ignored in [
            "",
            "ready",
            "pair 600",
            "pair x https://a/#pair=1",
            "pair 0 https://a/#pair=1",
            "pair 600 http://a/#pair=1",
            "pair 600 https://a/",
            "hello",
        ] {
            assert_eq!(g(ignored), None, "{ignored:?}");
        }
        // The protocol is stdout only; stderr is the human log.
        assert_eq!(
            classify(Program::Gateway, Stream::Stderr, "ready 127.0.0.1:6080"),
            None
        );
        assert_eq!(
            classify(
                Program::Gateway,
                Stream::Stderr,
                "enrolled browser key 0a0b-0c0d-0e0f-1011"
            ),
            Some(Event::Enrolled("0a0b-0c0d-0e0f-1011".into()))
        );
    }

    #[test]
    fn cloudflared_connection_lines() {
        let t = |line| classify(Program::Tunnel, Stream::Stderr, line);
        assert_eq!(
            t(
                "2026-11-04T12:00:00Z INF Registered tunnel connection connIndex=2 connection=abc event=0 ip=198.41.200.13 location=sin02 protocol=quic"
            ),
            Some(Event::Registered(2))
        );
        assert_eq!(
            t(
                "2026-11-04T12:00:00Z INF Unregistered tunnel connection connIndex=2 event=0 ip=198.41.200.13"
            ),
            Some(Event::Lost(Some(2)))
        );
        assert_eq!(
            t("2026-11-04T12:00:00Z ERR Connection terminated error=\"x\" connIndex=1"),
            Some(Event::Lost(Some(1)))
        );
        assert_eq!(
            t("2026-11-04T12:00:00Z ERR Lost connection with the edge"),
            Some(Event::Lost(None))
        );
        assert_eq!(
            t("2026-11-04T12:00:00Z INF Registering tunnel connection connIndex=0"),
            None
        );
        assert_eq!(
            t("2026-11-04T12:00:00Z INF Starting tunnel tunnelID=x"),
            None
        );
        // Without an index, a registration cannot be tracked.
        assert_eq!(t("INF Registered tunnel connection"), None);
    }

    /// Captured from cloudflared 2026.10.3 on Windows during the live run on
    /// 2026-11-05. Machine-specific addresses and identifiers are redacted;
    /// the banner, readiness messages, and their order are preserved.
    const QUICK_TUNNEL_OUTPUT: &str = include_str!("fixtures/cloudflared-quick-tunnel.txt");

    #[test]
    fn quick_tunnel_output_yields_its_url_once_and_its_connections() {
        // cloudflared logs to stderr; the supervisor also accepts stdout.
        for stream in [Stream::Stdout, Stream::Stderr] {
            let events: Vec<Event> = QUICK_TUNNEL_OUTPUT
                .lines()
                .filter_map(|line| classify(Program::Tunnel, stream, line))
                .collect();
            assert_eq!(
                events,
                [
                    Event::QuickTunnelUrl(
                        "https://brian-crm-produce-intelligence.trycloudflare.com".into()
                    ),
                    Event::Registered(0),
                ]
            );
        }
        let t = |line| classify(Program::Tunnel, Stream::Stderr, line);
        for ignored in [
            "INF |  https://a.b.trycloudflare.com  |",
            "INF |  https://Upper.trycloudflare.com  |",
            "INF |  http://plain.trycloudflare.com  |",
            "INF |  https://evil.trycloudflare.com.attacker.example  |",
            "INF |  https://x.trycloudflare.com/path  |",
            "INF see https://topvnc.owner.workers.dev",
        ] {
            assert_eq!(t(ignored), None, "{ignored}");
        }
        // The gateway's pipes never yield one.
        assert_eq!(
            classify(
                Program::Gateway,
                Stream::Stdout,
                "https://brian-crm-produce-intelligence.trycloudflare.com"
            ),
            None
        );
    }

    #[test]
    fn sanitize_hides_pairing_secrets_and_bounds_lines() {
        assert_eq!(
            sanitize("link https://a.workers.dev/#pair=SECRET123 done", 200),
            "link https://a.workers.dev/#pair=[hidden] done"
        );
        assert_eq!(
            sanitize("https://a.workers.dev/#pair=SECRET", 200),
            "https://a.workers.dev/#pair=[hidden]"
        );
        assert_eq!(
            sanitize("#pair=A x #pair=B", 200),
            "#pair=[hidden] x #pair=[hidden]"
        );
        let cut = sanitize(&format!("{}#pair=SECRET", "x".repeat(40)), 44);
        assert!(!cut.contains("SECRET") && !cut.contains("SEC"), "{cut}");
        assert_eq!(sanitize("a\tb\x1b[0mc", 200), "a b [0mc");
        assert_eq!(sanitize(&"y".repeat(600), 512).chars().count(), 512);
    }
}
