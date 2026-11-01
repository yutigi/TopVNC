//! Socket operations that TopVNC's `#![forbid(unsafe_code)]` crates cannot
//! express themselves. Each one is a single, documented system call.
#![deny(unsafe_op_in_unsafe_fn)]

use std::net::TcpStream;

/// Wakes a thread blocked reading from this socket handle. Call it after
/// `shutdown`, on the same `TcpStream` the reading thread uses.
///
/// On Linux and macOS, `shutdown` already wakes a blocked `read`, so this does
/// nothing. On Windows it does not: a `recv` that is already blocked keeps
/// waiting until the peer sends data or closes, which a vanished peer never
/// does. This cancels the handle's pending I/O, and the blocked read fails with
/// `WSAEINTR`. Data received before the shutdown is still read in order, and a
/// read that starts after the shutdown returns at once, so calling this before
/// the reader reaches `recv` is also safe.
pub fn interrupt_reads(stream: &TcpStream) {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::{Foundation::HANDLE, System::IO::CancelIoEx};

        let handle = stream.as_raw_socket() as usize as HANDLE;
        // SAFETY: `stream` is borrowed for the whole call, so the socket handle
        // is open. A null OVERLAPPED pointer cancels every pending operation on
        // the handle and passes no caller memory to the system. A failure (no
        // pending I/O) needs no handling.
        let _ = unsafe { CancelIoEx(handle, std::ptr::null()) };
    }
    #[cfg(not(windows))]
    let _ = stream;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (server, client)
    }

    /// Reads one byte at a time until the read fails or ends, then reports
    /// the bytes it got.
    fn spawn_reader(stream: Arc<TcpStream>) -> mpsc::Receiver<Vec<u8>> {
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut byte = [0];
            while let Ok(1) = (&*stream).read(&mut byte) {
                bytes.push(byte[0]);
            }
            let _ = done.send(bytes);
        });
        finished
    }

    #[test]
    fn a_blocked_reader_wakes_after_shutdown_while_the_peer_stays_silent() {
        let (server, mut client) = pair();
        client.write_all(b"ab").unwrap();
        let reader = Arc::new(server.try_clone().unwrap());
        let finished = spawn_reader(Arc::clone(&reader));
        // Let the reader consume the bytes and block in recv.
        std::thread::sleep(Duration::from_millis(100));

        server.shutdown(Shutdown::Both).unwrap();
        interrupt_reads(&reader);

        let bytes = finished
            .recv_timeout(Duration::from_secs(2))
            .expect("the reader stayed blocked while the peer kept the connection open");
        assert_eq!(bytes, b"ab");
        drop(client);
    }

    #[test]
    fn interrupting_before_the_reader_starts_still_ends_it() {
        let (server, _client) = pair();
        let reader = Arc::new(server.try_clone().unwrap());
        server.shutdown(Shutdown::Both).unwrap();
        interrupt_reads(&reader);
        let finished = spawn_reader(reader);
        assert!(
            finished
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn interrupting_without_a_pending_read_is_harmless() {
        let (server, mut client) = pair();
        interrupt_reads(&server);
        client.write_all(b"x").unwrap();
        let mut byte = [0];
        (&server).read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"x");
    }
}
