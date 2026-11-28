//! Native TCP/TLS transport. RFB parsing remains transport independent.
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
use rustls::{ClientConfig, ClientConnection, Connection, RootCertStore, ServerConnection};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// TLS 1.3 only. An explicit CA file replaces the public trust roots.
pub fn client_config(ca_file: Option<&Path>) -> io::Result<Arc<ClientConfig>> {
    if let Some(path) = ca_file {
        let certs = CertificateDer::pem_file_iter(path).map_err(invalid)?;
        return custom_client_config(certs.map(|cert| cert.map_err(invalid)));
    }
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    client_config_with_roots(roots)
}

fn custom_client_config(
    certs: impl IntoIterator<Item = io::Result<CertificateDer<'static>>>,
) -> io::Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    for cert in certs {
        roots.add(cert?).map_err(invalid)?;
    }
    if roots.is_empty() {
        return Err(invalid("TLS CA file contains no certificates"));
    }
    client_config_with_roots(roots)
}

fn client_config_with_roots(roots: RootCertStore) -> io::Result<Arc<ClientConfig>> {
    let config = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(invalid)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Load a PEM certificate chain and matching PKCS#1, PKCS#8, or SEC1 private key.
pub fn server_config(cert: &Path, key: &Path) -> io::Result<Arc<rustls::ServerConfig>> {
    let chain = CertificateDer::pem_file_iter(cert)
        .map_err(invalid)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)?;
    let key = PrivateKeyDer::from_pem_file(key).map_err(invalid)?;
    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(invalid)?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(invalid)?;
    Ok(Arc::new(config))
}

/// PEM directory name. Host credentials use the working directory, while
/// viewer trust uses the per-user configuration directory.
pub const LOCAL_CERT_DIR: &str = "certs.local";

/// Largest file inspected while detecting PEM files.
const MAX_DETECTED_PEM_BYTES: u64 = 1024 * 1024;
const MAX_VIEWER_PEM_FILES: usize = 128;

struct DiscoveredCa {
    path: PathBuf,
    chain: Vec<CertificateDer<'static>>,
}

impl DiscoveredCa {
    fn into_client(self, server_name: String) -> io::Result<(ClientTransport, PathBuf)> {
        let config = custom_client_config(self.chain.into_iter().map(Ok))
            .map_err(|error| discovery_error(&self.path, "load certificates from", error))?;
        Ok((
            ClientTransport::Tls {
                config,
                server_name,
            },
            self.path,
        ))
    }
}

fn discovery_error(path: &Path, action: &str, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "Viewer TLS trust: cannot {action} {}: {error}. Fix access or use --tls-ca FILE",
            path.display()
        ),
    )
}

fn read_viewer_pem(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(discovery_error(path, "open", error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| discovery_error(path, "inspect", error))?;
    if !metadata.is_file() || metadata.len() > MAX_DETECTED_PEM_BYTES {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(MAX_DETECTED_PEM_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| discovery_error(path, "read", error))?;
    Ok((bytes.len() as u64 <= MAX_DETECTED_PEM_BYTES).then_some(bytes))
}

/// Select a locally distributed host certificate by its DNS/IP identity.
/// Selection is not verification: the live TLS handshake must still validate
/// the chain, validity period, name, and proof of possession of the private key.
fn detect_client_ca(dir: &Path, name: &str) -> io::Result<Option<DiscoveredCa>> {
    let name = ServerName::try_from(name).map_err(invalid)?;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(discovery_error(dir, "list", error)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| discovery_error(dir, "list", error))?;
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pem"))
        {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_file() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(discovery_error(&path, "inspect", error)),
        }
        if paths.len() == MAX_VIEWER_PEM_FILES {
            return Err(invalid(format!(
                "Too many .pem files in {} (limit {MAX_VIEWER_PEM_FILES}). Remove some or use --tls-ca FILE",
                dir.display()
            )));
        }
        paths.push(path);
    }
    paths.sort();
    let mut selected: Option<DiscoveredCa> = None;
    for path in paths {
        let Some(bytes) = read_viewer_pem(&path)? else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        // Never use a combined certificate/key file as viewer trust.
        if text.contains("PRIVATE KEY-----") {
            continue;
        }
        let Ok(chain) = CertificateDer::pem_slice_iter(&bytes).collect::<Result<Vec<_>, _>>()
        else {
            continue;
        };
        let Some(leaf) = chain.first() else {
            continue;
        };
        let Ok(parsed) = rustls::server::ParsedCertificate::try_from(leaf) else {
            continue;
        };
        if rustls::client::verify_server_name(&parsed, &name).is_err() {
            continue;
        }
        if let Some(previous) = &selected {
            if previous.chain != chain {
                return Err(invalid(format!(
                    "Multiple certificates in {} match {}. Use --tls-ca FILE to choose one",
                    dir.display(),
                    name.to_str()
                )));
            }
        } else {
            selected = Some(DiscoveredCa { path, chain });
        }
    }
    Ok(selected)
}

/// Finds one host certificate and private key among the `.pem` files in
/// `dir` by their PEM labels, not their names. Returns `None` when there is no
/// private key, more than one, no certificate, or several different
/// certificates with none named after the key (`server-key.pem` ->
/// `server.pem`). Nothing is generated or trusted; the files are validated
/// when the host starts, exactly as if they were passed on the command line.
pub fn detect_host_credentials(dir: &Path) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let mut entries = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pem"))
                && std::fs::metadata(path)
                    .is_ok_and(|meta| meta.is_file() && meta.len() <= MAX_DETECTED_PEM_BYTES)
        })
        .collect::<Vec<_>>();
    entries.sort();
    let mut keys = Vec::new();
    let mut certs = Vec::new();
    for path in entries {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----") {
            keys.push(path);
        } else if text.contains("-----BEGIN CERTIFICATE-----") {
            certs.push((path, text));
        }
    }
    let [key] = keys.as_slice() else {
        return None;
    };
    let stem = key.file_stem()?.to_str()?;
    let base = ["-key", "_key", ".key", "-privkey", "_privkey"]
        .iter()
        .find_map(|suffix| stem.strip_suffix(suffix))
        .unwrap_or(stem);
    if let Some((cert, _)) = certs.iter().find(|(path, _)| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s != stem && s.eq_ignore_ascii_case(base))
    }) {
        return Some((cert.clone(), key.clone()));
    }
    let (first, text) = certs.first()?;
    certs
        .iter()
        .all(|(_, other)| other == text)
        .then(|| (first.clone(), key.clone()))
}

/// Explicit native viewer transport. The executable defaults to TLS; legacy
/// library connection APIs retain TCP for gateways and third-party integrations.
#[derive(Clone)]
pub enum ClientTransport {
    Tls {
        config: Arc<ClientConfig>,
        server_name: String,
    },
    Unencrypted,
}

/// Native executable transport settings. Deliberately not persisted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsOptions {
    pub unencrypted: bool,
    pub cert: Option<std::path::PathBuf>,
    pub key: Option<std::path::PathBuf>,
    pub ca: Option<std::path::PathBuf>,
    pub server_name: Option<String>,
}

impl TlsOptions {
    /// Prepare the transport for one connection and report the discovered PEM,
    /// if any. Explicit trust and plaintext choices bypass discovery.
    pub fn client_with_local_ca(
        &self,
        host: &str,
        dir: Option<&Path>,
    ) -> io::Result<(ClientTransport, Option<PathBuf>)> {
        if let Some(dir) = dir
            && !self.unencrypted
            && self.ca.is_none()
        {
            let server_name = self.client_server_name(host);
            if let Some(ca) = detect_client_ca(dir, &server_name)? {
                let (transport, path) = ca.into_client(server_name)?;
                return Ok((transport, Some(path)));
            }
        }
        Ok((self.client(host)?, None))
    }

    fn client_server_name(&self, host: &str) -> String {
        self.server_name.clone().unwrap_or_else(|| {
            host.trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned()
        })
    }

    pub fn label(&self) -> &'static str {
        if self.unencrypted {
            "TCP / UNENCRYPTED (EXPLICIT)"
        } else {
            "TLS 1.3 / VERIFIED SERVER"
        }
    }

    pub fn client(&self, host: &str) -> io::Result<ClientTransport> {
        if self.unencrypted {
            if self.ca.is_some() || self.server_name.is_some() {
                return Err(invalid(
                    "--unencrypted cannot be combined with TLS trust options",
                ));
            }
            return Ok(ClientTransport::Unencrypted);
        }
        Ok(ClientTransport::Tls {
            config: client_config(self.ca.as_deref())?,
            server_name: self.client_server_name(host),
        })
    }

    pub fn server(&self) -> io::Result<Option<Arc<rustls::ServerConfig>>> {
        if self.unencrypted {
            if self.cert.is_some() || self.key.is_some() {
                return Err(invalid(
                    "--unencrypted cannot be combined with TLS credentials",
                ));
            }
            return Ok(None);
        }
        match (&self.cert, &self.key) {
            (Some(cert), Some(key)) => server_config(cert, key).map(Some),
            _ => Err(invalid(
                "TLS hosting needs --tls-cert and --tls-key or PEMs in certs.local, or PLAIN TCP (NO TLS) on a trusted network.",
            )),
        }
    }
}

/// Clones share TLS state, ordered ciphertext writes, and one socket handle,
/// so `interrupt_reads` on any clone wakes a read blocked on another. Only one
/// clone may read at a time, as with a split TCP connection. Blocking socket
/// reads happen outside the TLS lock, so an idle framebuffer reader cannot
/// delay input.
pub(crate) struct NativeStream {
    socket: Arc<TcpStream>,
    tls: Option<Arc<Mutex<Connection>>>,
    received: Arc<Mutex<Received>>,
    /// Absolute bound on every socket operation while set; see
    /// [`NativeStream::set_deadline`]. Not shared with clones.
    deadline: Option<Instant>,
}

#[derive(Default)]
struct Received {
    bytes: Vec<u8>,
    offset: usize,
    end: usize,
}

impl From<TcpStream> for NativeStream {
    fn from(socket: TcpStream) -> Self {
        Self::plain(Arc::new(socket))
    }
}

impl NativeStream {
    fn plain(socket: Arc<TcpStream>) -> Self {
        Self {
            socket,
            tls: None,
            received: Arc::default(),
            deadline: None,
        }
    }

    pub fn client(
        socket: impl Into<Arc<TcpStream>>,
        transport: ClientTransport,
        deadline: Instant,
    ) -> io::Result<Self> {
        match transport {
            ClientTransport::Unencrypted => Ok(Self::plain(socket.into())),
            ClientTransport::Tls {
                config,
                server_name,
            } => {
                let name = ServerName::try_from(server_name).map_err(invalid)?;
                let connection = ClientConnection::new(config, name).map_err(invalid)?;
                Self::handshake(socket.into(), Connection::Client(connection), deadline)
            }
        }
    }

    pub fn server(
        socket: impl Into<Arc<TcpStream>>,
        config: Option<Arc<rustls::ServerConfig>>,
        deadline: Instant,
    ) -> io::Result<Self> {
        match config {
            None => Ok(Self::plain(socket.into())),
            Some(config) => {
                let connection = ServerConnection::new(config).map_err(invalid)?;
                Self::handshake(socket.into(), Connection::Server(connection), deadline)
            }
        }
    }

    fn handshake(
        socket: Arc<TcpStream>,
        mut connection: Connection,
        deadline: Instant,
    ) -> io::Result<Self> {
        socket.set_nodelay(true)?;
        connection.set_buffer_limit(Some(64 * 1024));
        let is_client = matches!(connection, Connection::Client(_));
        let mut io = DeadlineSocket {
            socket: &socket,
            deadline,
            greeting: Vec::new(),
        };
        while connection.is_handshaking() {
            // complete_io can perform several reads internally. Bound every
            // operation by one deadline, including a peer trickling bytes.
            if let Err(error) = connection.complete_io(&mut io) {
                // rustls reports a plaintext greeting as a corrupt record.
                // Name the real cause; never fall back to plaintext.
                if is_client && io.greeting.starts_with(RFB_GREETING) {
                    let _ = socket.shutdown(Shutdown::Both);
                    return Err(invalid(PLAINTEXT_SERVER));
                }
                return Err(error);
            }
        }
        Ok(Self {
            socket,
            tls: Some(Arc::new(Mutex::new(connection))),
            received: Arc::default(),
            deadline: None,
        })
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            socket: Arc::clone(&self.socket),
            tls: self.tls.clone(),
            received: self.received.clone(),
            deadline: None,
        })
    }

    /// Bounds every blocking socket operation this handle performs, including
    /// ciphertext reads and record flushes inside one TLS read or write, by an
    /// absolute deadline. A peer trickling a partial record therefore cannot
    /// restart a relative timeout. `None` ends the bound but leaves the last
    /// socket timeouts in place; callers then set their session timeouts.
    pub(crate) fn set_deadline(&mut self, deadline: Option<Instant>) {
        self.deadline = deadline;
    }

    fn io(&self) -> BoundedSocket<'_> {
        BoundedSocket {
            socket: &self.socket,
            deadline: self.deadline,
        }
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.socket.shutdown(how)
    }
    /// Wakes a read blocked on any clone of this stream. Call it after
    /// `shutdown`; Windows does not wake a blocked `recv` on shutdown alone.
    pub fn interrupt_reads(&self) {
        topvnc_net::interrupt_reads(&self.socket);
    }
    pub fn set_nodelay(&self, value: bool) -> io::Result<()> {
        self.socket.set_nodelay(value)
    }
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.socket.set_read_timeout(timeout)
    }
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.socket.set_write_timeout(timeout)
    }

    fn flush_records(connection: &mut Connection, mut io: BoundedSocket) -> io::Result<()> {
        let socket = io.socket;
        let result = (|| {
            while connection.wants_write() {
                if connection.write_tls(&mut io)? == 0 {
                    return Err(io::ErrorKind::WriteZero.into());
                }
            }
            Ok(())
        })();
        if result.is_err() {
            // Partial ciphertext writes cannot be retried as new records.
            let _ = socket.shutdown(Shutdown::Both);
        }
        result
    }
}

impl Read for NativeStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let Some(tls) = &self.tls else {
            return self.io().read(bytes);
        };
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            {
                let mut connection = tls.lock().map_err(|_| invalid("TLS lock poisoned"))?;
                match connection.reader().read(bytes) {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    result => return result,
                }
            }
            // Keep ciphertext that rustls cannot consume yet. Processing one
            // record can fill its plaintext buffer; drain that before feeding
            // the next record. Clones share this bounded receive buffer.
            let mut received = self
                .received
                .lock()
                .map_err(|_| invalid("TLS receive lock poisoned"))?;
            if received.offset == received.end {
                received.bytes.resize(16 * 1024, 0);
                received.end = self.io().read(&mut received.bytes)?;
                received.offset = 0;
            }
            let mut connection = tls.lock().map_err(|_| invalid("TLS lock poisoned"))?;
            let consumed =
                connection.read_tls(&mut &received.bytes[received.offset..received.end])?;
            received.offset += consumed;
            if let Err(error) = connection.process_new_packets() {
                let _ = Self::flush_records(&mut connection, self.io());
                let _ = self.socket.shutdown(Shutdown::Both);
                return Err(invalid(error));
            }
            Self::flush_records(&mut connection, self.io())?;
        }
    }
}

impl Write for NativeStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(tls) = &self.tls else {
            return self.io().write(bytes);
        };
        let mut connection = tls.lock().map_err(|_| invalid("TLS lock poisoned"))?;
        let count = connection
            .writer()
            .write(&bytes[..bytes.len().min(16 * 1024)])?;
        if let Err(error) = Self::flush_records(&mut connection, self.io()) {
            // A partial ciphertext write is terminal. Never retry plaintext
            // with a new nonce after a socket timeout.
            let _ = self.socket.shutdown(Shutdown::Both);
            return Err(error);
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(tls) = &self.tls {
            let mut connection = tls.lock().map_err(|_| invalid("TLS lock poisoned"))?;
            Self::flush_records(&mut connection, self.io())?;
        }
        (&*self.socket).flush()
    }
}

/// A plain RFB server opens with "RFB 003.00x\n" where TLS expects a record.
const RFB_GREETING: &[u8] = b"RFB ";
/// Kept to two lines of the settings status area; see docs/native-tls.md.
pub const PLAINTEXT_SERVER: &str = "Server answered with unencrypted VNC, not TLS. \
     Enable TLS on the host, or use --unencrypted on a trusted network only.";

fn remaining(deadline: Instant, message: &'static str) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, message))
}

/// One socket operation at a time. While a deadline is set, each operation
/// waits at most for the time left, recomputed immediately before it blocks.
#[derive(Clone, Copy)]
struct BoundedSocket<'a> {
    socket: &'a TcpStream,
    deadline: Option<Instant>,
}
impl Read for BoundedSocket<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if let Some(deadline) = self.deadline {
            let left = remaining(deadline, "Connection handshake timed out")?;
            self.socket.set_read_timeout(Some(left))?;
        }
        let mut socket = self.socket;
        socket.read(bytes)
    }
}
impl Write for BoundedSocket<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(deadline) = self.deadline {
            let left = remaining(deadline, "Connection handshake timed out")?;
            self.socket.set_write_timeout(Some(left))?;
        }
        let mut socket = self.socket;
        socket.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        let mut socket = self.socket;
        socket.flush()
    }
}

struct DeadlineSocket<'a> {
    socket: &'a TcpStream,
    deadline: Instant,
    /// The peer's first bytes, kept only to explain a failed handshake.
    greeting: Vec<u8>,
}
impl DeadlineSocket<'_> {
    fn remaining(&self) -> io::Result<Duration> {
        remaining(self.deadline, "TLS handshake timed out")
    }
}
impl Read for DeadlineSocket<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.socket.set_read_timeout(Some(self.remaining()?))?;
        let mut socket = self.socket;
        let count = socket.read(bytes)?;
        let wanted = RFB_GREETING.len().saturating_sub(self.greeting.len());
        self.greeting.extend_from_slice(&bytes[..count.min(wanted)]);
        Ok(count)
    }
}
impl Write for DeadlineSocket<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.socket.set_write_timeout(Some(self.remaining()?))?;
        let mut socket = self.socket;
        socket.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        let mut socket = self.socket;
        socket.flush()
    }
}

#[cfg(test)]
#[path = "../tests/native/transport.rs"]
mod tests;
