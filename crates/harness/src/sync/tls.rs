//! Certificate trust for git over HTTPS on Android (docs/sync-build.md, "Crate
//! and auth").
//!
//! libgit2 verifies HTTPS with OpenSSL. On Android that OpenSSL is built from
//! source by `openssl-src`, which configures it with `no-stdio`: `BIO_new_file`
//! is compiled to return NULL, so OpenSSL can read no certificate file or
//! folder at all, trusts nothing, and every remote fails with "the SSL
//! certificate is invalid". Pointing it at a bundle (`OPENSSLDIR`,
//! `SSL_CERT_FILE`) cannot work for the same reason.
//!
//! So on Android trust is decided here, in libgit2's certificate callback,
//! with rustls and the Mozilla roots that WebDAV and the model clients already
//! use on the phone. The callback receives only the leaf certificate libgit2
//! got from the server; its intermediates come from a rustls handshake with
//! the same host, and that leaf is then verified against them: a chain to a
//! trusted root, the validity period, server-auth usage and the host name. A
//! connection is accepted only when that passes, so libgit2 never talks to a
//! server rustls would refuse.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::ServerCertVerifier;
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, RootCertStore};

/// Connecting, and each read or write of the handshake.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The Mozilla roots bundled with the app (`webpki-roots`), as rustls wants
/// them.
pub(super) fn mozilla_roots() -> Arc<RootCertStore> {
    Arc::new(RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    })
}

/// Verify `leaf` (DER, the certificate libgit2 received) for `host`, trusting
/// `roots`. The error is a short reason for the learner, e.g. "invalid peer
/// certificate: UnknownIssuer".
pub(super) fn verify_leaf(
    leaf: &[u8],
    host: &str,
    port: u16,
    roots: Arc<RootCertStore>,
) -> Result<(), String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let name = ServerName::try_from(host.to_owned())
        .map_err(|_| format!("{host} is not a valid host name"))?;
    let intermediates = server_intermediates(host, port, &name, &roots, &provider)?;
    let verifier = WebPkiServerVerifier::builder_with_provider(roots, provider)
        .build()
        .map_err(|e| format!("the certificate check could not be set up: {e}"))?;
    verifier
        .verify_server_cert(
            &CertificateDer::from(leaf),
            &intermediates,
            &name,
            &[],
            UnixTime::now(),
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The certificates after the leaf that `host` sends a rustls client. The
/// handshake verifies the server too, so a host rustls refuses fails here
/// with rustls's reason.
fn server_intermediates(
    host: &str,
    port: u16,
    name: &ServerName<'static>,
    roots: &Arc<RootCertStore>,
    provider: &Arc<CryptoProvider>,
) -> Result<Vec<CertificateDer<'static>>, String> {
    let config = ClientConfig::builder_with_provider(Arc::clone(provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(Arc::clone(roots))
        .with_no_client_auth();
    let mut tls =
        ClientConnection::new(Arc::new(config), name.clone()).map_err(|e| e.to_string())?;
    let mut socket = connect(host, port).map_err(|e| format!("could not reach {host}: {e}"))?;
    while tls.is_handshaking() {
        tls.complete_io(&mut socket).map_err(|e| e.to_string())?;
    }
    let chain = tls
        .peer_certificates()
        .ok_or_else(|| "the server sent no certificate".to_owned())?;
    Ok(chain.iter().skip(1).cloned().collect())
}

/// The first address of `host` that accepts a connection in time.
fn connect(host: &str, port: u16) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address found");
    for address in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, TIMEOUT) {
            Ok(socket) => {
                socket.set_read_timeout(Some(TIMEOUT))?;
                socket.set_write_timeout(Some(TIMEOUT))?;
                return Ok(socket);
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::PrivateKeyDer;
    use rustls::{ServerConfig, ServerConnection};
    use std::net::TcpListener;

    // A private CA and a `localhost` certificate it signed, valid until 2126
    // (testdata/, made with the openssl CLI; the key never leaves the tests).
    const CA: &[u8] = include_bytes!("testdata/test-ca.der");
    const LEAF: &[u8] = include_bytes!("testdata/localhost.der");
    const LEAF_KEY: &[u8] = include_bytes!("testdata/localhost.key.der");

    /// A TLS server on 127.0.0.1 presenting the `localhost` certificate to
    /// every connection; returns its port.
    fn server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = Arc::new(
            ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from(LEAF.to_vec())],
                    PrivateKeyDer::try_from(LEAF_KEY.to_vec()).unwrap(),
                )
                .unwrap(),
        );
        std::thread::spawn(move || {
            for socket in listener.incoming() {
                let Ok(mut socket) = socket else { continue };
                let mut tls = ServerConnection::new(Arc::clone(&config)).unwrap();
                while tls.is_handshaking() {
                    if tls.complete_io(&mut socket).is_err() {
                        break;
                    }
                }
            }
        });
        port
    }

    fn test_roots() -> Arc<RootCertStore> {
        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from(CA.to_vec())).unwrap();
        Arc::new(roots)
    }

    #[test]
    fn a_certificate_from_a_trusted_root_for_the_host_is_accepted() {
        let port = server();
        assert_eq!(verify_leaf(LEAF, "localhost", port, test_roots()), Ok(()));
    }

    #[test]
    fn an_untrusted_issuer_is_refused_with_its_reason() {
        // The phone's case if something intercepted the connection: the
        // certificate chains to a root the app doesn't trust.
        let port = server();
        let error = verify_leaf(LEAF, "localhost", port, mozilla_roots()).unwrap_err();
        assert!(error.contains("UnknownIssuer"), "{error}");
    }

    #[test]
    fn a_certificate_for_another_name_is_refused() {
        let port = server();
        let error = verify_leaf(LEAF, "127.0.0.1", port, test_roots()).unwrap_err();
        assert!(error.contains("not valid for name"), "{error}");
    }

    #[test]
    fn a_leaf_other_than_the_servers_valid_one_is_refused() {
        // libgit2 handed over a certificate the server's real chain doesn't
        // vouch for (here the CA certificate itself posing as a leaf).
        let port = server();
        assert!(verify_leaf(CA, "localhost", port, test_roots()).is_err());
    }
}
