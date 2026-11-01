//! Native TCP/TLS transport. RFB parsing remains transport independent.
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject};
use rustls::{ClientConfig, ClientConnection, Connection, RootCertStore, ServerConnection};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
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
    let mut roots = RootCertStore::empty();
    if let Some(path) = ca_file {
        let certs = CertificateDer::pem_file_iter(path).map_err(invalid)?;
        for cert in certs {
            roots.add(cert.map_err(invalid)?).map_err(invalid)?;
        }
        if roots.is_empty() {
            return Err(invalid("TLS CA file contains no certificates"));
        }
    } else {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
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
            server_name: self.server_name.clone().unwrap_or_else(|| {
                host.trim()
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_owned()
            }),
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
                "TLS hosting needs --tls-cert CERT.pem and --tls-key KEY.pem",
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
        while connection.is_handshaking() {
            // complete_io can perform several reads internally. Bound every
            // operation by one deadline, including a peer trickling bytes.
            connection.complete_io(&mut DeadlineSocket {
                socket: &socket,
                deadline,
            })?;
        }
        Ok(Self {
            socket,
            tls: Some(Arc::new(Mutex::new(connection))),
            received: Arc::default(),
        })
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            socket: Arc::clone(&self.socket),
            tls: self.tls.clone(),
            received: self.received.clone(),
        })
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

    fn flush_records(connection: &mut Connection, socket: &TcpStream) -> io::Result<()> {
        let result = (|| {
            while connection.wants_write() {
                if connection.write_tls(&mut &*socket)? == 0 {
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
            return (&*self.socket).read(bytes);
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
                received.end = (&*self.socket).read(&mut received.bytes)?;
                received.offset = 0;
            }
            let mut connection = tls.lock().map_err(|_| invalid("TLS lock poisoned"))?;
            let consumed =
                connection.read_tls(&mut &received.bytes[received.offset..received.end])?;
            received.offset += consumed;
            if let Err(error) = connection.process_new_packets() {
                let _ = Self::flush_records(&mut connection, &self.socket);
                let _ = self.socket.shutdown(Shutdown::Both);
                return Err(invalid(error));
            }
            Self::flush_records(&mut connection, &self.socket)?;
        }
    }
}

impl Write for NativeStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(tls) = &self.tls else {
            return (&*self.socket).write(bytes);
        };
        let mut connection = tls.lock().map_err(|_| invalid("TLS lock poisoned"))?;
        let count = connection
            .writer()
            .write(&bytes[..bytes.len().min(16 * 1024)])?;
        if let Err(error) = Self::flush_records(&mut connection, &self.socket) {
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
            Self::flush_records(&mut connection, &self.socket)?;
        }
        (&*self.socket).flush()
    }
}

struct DeadlineSocket<'a> {
    socket: &'a TcpStream,
    deadline: Instant,
}
impl DeadlineSocket<'_> {
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))
    }
}
impl Read for DeadlineSocket<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.socket.set_read_timeout(Some(self.remaining()?))?;
        let mut socket = self.socket;
        socket.read(bytes)
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
