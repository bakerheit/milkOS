//! `https://`: TLS 1.3 (the shared `tls` crate) between [`crate::http`]'s client and a TCP
//! [`Transport`]. [`secure`] is what a [`crate::http::Net::secure`] calls: the handshake runs
//! there, so certificate problems come back as [`Error::Certificate`] (the Browser's "can't be
//! trusted" page) and everything else TLS as [`Error::Tls`], both in `tls`'s plain words.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::ToString;

use crate::http::{Error, Transport};

/// The TCP connection under TLS: milkweb's transport as `tls`'s.
struct Tcp(Box<dyn Transport>);

impl tls::Transport for Tcp {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, tls::IoError> {
        self.0.read(buf).map_err(io)
    }
    fn write_all(&mut self, mut data: &[u8]) -> Result<(), tls::IoError> {
        while !data.is_empty() {
            match self.0.write(data).map_err(io)? {
                0 => return Err(tls::IoError::Failed("the connection closed".to_string())),
                n => data = &data[n.min(data.len())..],
            }
        }
        Ok(())
    }
}

fn io(e: Error) -> tls::IoError {
    match e {
        Error::Timeout => tls::IoError::TimedOut,
        e => tls::IoError::Failed(e.to_string()),
    }
}

/// A TLS stream as milkweb's transport.
struct Secure(tls::Stream<Tcp>);

/// `tls`'s error as milkweb's, for `host`.
pub fn error(host: &str, e: tls::TlsError) -> Error {
    match e {
        tls::TlsError::Io(tls::IoError::TimedOut) => Error::Timeout,
        tls::TlsError::Io(tls::IoError::Failed(m)) => Error::Io(m),
        tls::TlsError::Certificate(c) => Error::Certificate(format!("{host}: {c}")),
        e => Error::Tls(format!("{host}: {e}")),
    }
}

impl Transport for Secure {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        match self.0.read(buf) {
            Ok(n) => Ok(n),
            // Many servers close without close_notify; HTTP's own framing (Content-Length,
            // chunked) still tells a whole body from a cut one.
            Err(tls::TlsError::Truncated) => Ok(0),
            Err(e) => {
                let host = match &self.0.info().server_name {
                    tls::ServerName::Dns(d) => d.clone(),
                    tls::ServerName::Ip(_) => "the server".to_string(),
                };
                Err(error(&host, e))
            }
        }
    }
    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        self.0.write(buf).map_err(|e| error("the server", e))
    }
}

/// Run the TLS handshake for `host` over `tcp` with `client` (its trust store, clock and
/// random numbers): the stream to speak HTTP over.
pub fn secure(
    client: &tls::Client,
    host: &str,
    tcp: Box<dyn Transport>,
) -> Result<Box<dyn Transport>, Error> {
    secure_info(client, host, tcp).map(|(t, _)| t)
}

/// [`secure`], and what was negotiated (the cipher suite, the server's certificates), for a
/// padlock's "connection details".
pub fn secure_info(
    client: &tls::Client,
    host: &str,
    tcp: Box<dyn Transport>,
) -> Result<(Box<dyn Transport>, tls::ConnectionInfo), Error> {
    let s = client.connect(host, Tcp(tcp)).map_err(|e| error(host, e))?;
    let info = s.info().clone();
    Ok((Box::new(Secure(s)), info))
}
