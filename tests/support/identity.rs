use super::ClientTransport;
use std::sync::Arc;

pub struct Identity {
    pub cert: rcgen::Certificate,
    pub key: rcgen::KeyPair,
}

impl Identity {
    pub fn new() -> Self {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
                .unwrap();
        Self {
            cert,
            key: signing_key,
        }
    }

    pub fn expired() -> Self {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
        params.not_before = rcgen::date_time_ymd(2019, 1, 1);
        params.not_after = rcgen::date_time_ymd(2020, 1, 1);
        let cert = params.self_signed(&key).unwrap();
        Self { cert, key }
    }

    pub fn server_config(&self) -> Arc<rustls::ServerConfig> {
        Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![self.cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(self.key.serialize_der()).into(),
            )
            .unwrap(),
        )
    }

    pub fn client(&self, server_name: &str) -> ClientTransport {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(self.cert.der().clone()).unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        ClientTransport::Tls {
            config: Arc::new(config),
            server_name: server_name.into(),
        }
    }
}
