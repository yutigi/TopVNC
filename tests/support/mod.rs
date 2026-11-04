use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use topvnc::{Framebuffer, ServerConfig, VncServer};

use topvnc::transport::ClientTransport;
mod identity;
pub use identity::Identity;

pub struct Host {
    pub server: Arc<VncServer>,
    runner: Option<JoinHandle<io::Result<()>>>,
}

impl Host {
    pub fn start(tls: Option<Arc<rustls::ServerConfig>>, framebuffer: Framebuffer) -> Self {
        Self::configured(
            framebuffer,
            ServerConfig {
                tls,
                password: Some("native-test-password".into()),
                name: "TopVNC encrypted native automation fixture".into(),
                ..Default::default()
            },
        )
    }

    pub fn configured(framebuffer: Framebuffer, config: ServerConfig) -> Self {
        let server = Arc::new(VncServer::bind("127.0.0.1:0", framebuffer, config).unwrap());
        let copy = server.clone();
        Self {
            server,
            runner: Some(thread::spawn(move || copy.run())),
        }
    }

    pub fn address(&self) -> SocketAddr {
        self.server.local_addr().unwrap()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.server.stop();
        self.runner.take().unwrap().join().unwrap().unwrap();
    }
}
