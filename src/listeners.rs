//! The sockets a [`VncServer`](crate::VncServer) accepts viewers on. One
//! server, with one desktop capture, can listen on several addresses, each
//! with its own transport security and password: for example TLS on the
//! local network for native viewers and plain RFB on loopback for the
//! browser gateway. Listeners can be added and removed while the server runs.

use crate::{ServerSessions, invalid};
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};

/// One listener of a [`ServerListeners`] set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ListenerId(u64);

impl ListenerId {
    /// A listener for registry tests that need no socket.
    #[cfg(test)]
    pub(crate) const TEST: Self = Self(0);
}

/// How viewers that arrive on one listener connect and authenticate.
#[derive(Clone, Default)]
pub struct ListenerSecurity {
    /// TLS before RFB; `None` is plain TCP.
    pub tls: Option<Arc<rustls::ServerConfig>>,
    /// VNC password; `None` only with `allow_insecure`.
    pub password: Option<String>,
    pub allow_insecure: bool,
}

impl ListenerSecurity {
    fn check(&self) -> io::Result<()> {
        if self
            .password
            .as_ref()
            .is_some_and(|password| password.is_empty())
        {
            return Err(invalid("server password must not be empty"));
        }
        if self.password.is_none() && !self.allow_insecure {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "server requires a password unless allow_insecure is explicitly enabled",
            ));
        }
        Ok(())
    }
}

/// A listening socket and the security of the viewers it accepts.
pub(crate) struct Listener {
    pub(crate) id: ListenerId,
    socket: TcpListener,
    pub(crate) tls: Option<Arc<rustls::ServerConfig>>,
    pub(crate) password: Option<String>,
}

#[derive(Default)]
struct Table {
    listeners: Vec<Listener>,
    next_id: u64,
    /// The listener the server was bound with.
    primary: Option<ListenerId>,
    /// The server stopped; its sockets are closed and none can be added.
    closed: bool,
}

/// The listeners of one server. Clones share the set, so a host can add or
/// remove listeners from another thread while the server accepts viewers.
#[derive(Clone)]
pub struct ServerListeners {
    table: Arc<Mutex<Table>>,
    sessions: Arc<Mutex<ServerSessions>>,
}

impl std::fmt::Debug for ServerListeners {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerListeners")
            .finish_non_exhaustive()
    }
}

/// Handles are equal when they share one set.
impl PartialEq for ServerListeners {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.table, &other.table)
    }
}

impl Eq for ServerListeners {}

impl ServerListeners {
    pub(crate) fn new(sessions: Arc<Mutex<ServerSessions>>) -> Self {
        Self {
            table: Arc::new(Mutex::new(Table::default())),
            sessions,
        }
    }

    fn table(&self) -> io::Result<std::sync::MutexGuard<'_, Table>> {
        self.table
            .lock()
            .map_err(|_| invalid("listener table lock is poisoned"))
    }

    /// Listen on `address` for viewers that use `security`.
    pub fn add(
        &self,
        address: impl ToSocketAddrs,
        security: ListenerSecurity,
    ) -> io::Result<ListenerId> {
        security.check()?;
        let mut table = self.table()?;
        if table.closed {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "the server has stopped",
            ));
        }
        let socket = TcpListener::bind(address)?;
        socket.set_nonblocking(true)?;
        table.next_id += 1;
        let id = ListenerId(table.next_id);
        table.listeners.push(Listener {
            id,
            socket,
            tls: security.tls,
            password: security.password,
        });
        Ok(id)
    }

    pub(crate) fn add_primary(
        &self,
        address: impl ToSocketAddrs,
        security: ListenerSecurity,
    ) -> io::Result<ListenerId> {
        let id = self.add(address, security)?;
        self.table()?.primary = Some(id);
        Ok(id)
    }

    /// Stop listening on `id` and disconnect the viewers that came through
    /// it. Viewers of other listeners stay connected. Returns whether the
    /// listener was open.
    pub fn remove(&self, id: ListenerId) -> bool {
        let Ok(mut table) = self.table() else {
            return false;
        };
        let Some(index) = table
            .listeners
            .iter()
            .position(|listener| listener.id == id)
        else {
            return false;
        };
        // Dropping the socket stops new connections.
        table.listeners.remove(index);
        // Still under the table lock, so no client of this listener can be
        // registered after the sweep.
        if let Ok(sessions) = self.sessions.lock() {
            sessions.shutdown_listener(id);
        }
        true
    }

    /// The bound address of an open listener.
    pub fn local_addr(&self, id: ListenerId) -> Option<SocketAddr> {
        let table = self.table().ok()?;
        let listener = table.listeners.iter().find(|listener| listener.id == id)?;
        listener.socket.local_addr().ok()
    }

    /// The listener the server was bound with, while it is open.
    pub fn primary(&self) -> Option<ListenerId> {
        let table = self.table().ok()?;
        let primary = table.primary?;
        table
            .listeners
            .iter()
            .any(|listener| listener.id == primary)
            .then_some(primary)
    }

    /// Close every socket and refuse new listeners.
    pub(crate) fn close(&self) {
        if let Ok(mut table) = self.table() {
            table.closed = true;
            table.listeners.clear();
        }
    }

    /// Accept at most one pending connection per listener, passing each to
    /// `admit` while the set is locked so a concurrent [`Self::remove`]
    /// cannot miss it. Returns whether any connection arrived.
    pub(crate) fn accept_each(
        &self,
        mut admit: impl FnMut(TcpStream, &Listener) -> io::Result<()>,
    ) -> io::Result<bool> {
        let table = self.table()?;
        let mut accepted = false;
        for listener in &table.listeners {
            let stream = match listener.socket.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                // A client that resets before it is accepted affects only
                // that connection; keep listening for others.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            accepted = true;
            if stream.set_nonblocking(false).is_err() {
                continue;
            }
            admit(stream, listener)?;
        }
        Ok(accepted)
    }
}

#[cfg(test)]
mod tests;
