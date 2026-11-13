//! The native transport: TLS with rustls over `std::net`, the WebSocket
//! with tungstenite ([`NativeOpener`]), and an HTTPS GET by hand
//! ([`https_get`]: `Connection: close`, gzip). Native only: the browser
//! has its own WebSocket (`browser.rs`) and `fetch`.

use std::{
    io::{Read as _, Write as _},
    net::{TcpStream, ToSocketAddrs as _},
    sync::Arc,
    time::Duration,
};

use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use tungstenite::{
    Message as WsMessage, WebSocket,
    client::IntoClientRequest as _,
    http::{HeaderName, HeaderValue, header},
};

use super::socket::{Opener, Socket, SocketEvent};

/// The largest HTTPS response read.
const MAX_RESPONSE: usize = 32 << 20;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the handshakes and an HTTPS response may take.
const IO_TIMEOUT: Duration = Duration::from_secs(20);

type TlsStream = StreamOwned<ClientConnection, TcpStream>;

/// rustls with the platform's root certificates and the `ring` provider.
pub fn tls_config() -> Result<Arc<ClientConfig>, String> {
    let mut roots = RootCertStore::empty();
    let found = rustls_native_certs::load_native_certs();
    let (added, _) = roots.add_parsable_certificates(found.certs);
    if added == 0 {
        return Err(format!("no root certificates ({:?})", found.errors));
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// A TLS connection to `host`:443.
fn connect_tls(config: &Arc<ClientConfig>, host: &str) -> Result<TlsStream, String> {
    let address = (host, 443)
        .to_socket_addrs()
        .map_err(|error| format!("{host}: {error}"))?
        .next()
        .ok_or_else(|| format!("{host}: no address"))?;
    let tcp = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
        .map_err(|error| format!("{host}: {error}"))?;
    tcp.set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| tcp.set_write_timeout(Some(IO_TIMEOUT)))
        .and_then(|()| tcp.set_nodelay(true))
        .map_err(|error| error.to_string())?;
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|error| error.to_string())?;
    let connection =
        ClientConnection::new(config.clone(), name).map_err(|error| error.to_string())?;
    Ok(StreamOwned::new(connection, tcp))
}

/// The host of an `https://` or `wss://` URL.
pub fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split('/').next()?;
    Some(authority.split(':').next().unwrap_or(authority))
}

/// tungstenite over rustls, opened at once, with the same extra headers on
/// every handshake (who connects, where the service asks for it).
pub struct NativeOpener {
    config: Option<Arc<ClientConfig>>,
    headers: Vec<(&'static str, &'static str)>,
}

impl NativeOpener {
    pub fn new(headers: &[(&'static str, &'static str)]) -> Self {
        Self {
            config: None,
            headers: headers.to_vec(),
        }
    }

    /// The TLS configuration, made once.
    pub fn config(&mut self) -> Result<Arc<ClientConfig>, String> {
        if let Some(config) = &self.config {
            return Ok(config.clone());
        }
        let config = tls_config()?;
        self.config = Some(config.clone());
        Ok(config)
    }

    /// One HTTPS GET of `url` with the opener's headers: the body,
    /// unzipped.
    pub fn get(&mut self, url: &str) -> Result<Vec<u8>, String> {
        let config = self.config()?;
        https_get(&config, url, &self.headers)
    }
}

impl Opener for NativeOpener {
    fn open(&mut self, url: &str, protocol: &str) -> Result<Box<dyn Socket>, String> {
        let config = self.config()?;
        let host = host_of(url).ok_or_else(|| format!("{url}: no host"))?;
        let stream = connect_tls(&config, host)?;
        let mut request = url
            .into_client_request()
            .map_err(|error| error.to_string())?;
        let headers = request.headers_mut();
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(protocol).map_err(|error| error.to_string())?,
        );
        for (name, value) in &self.headers {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).map_err(|error| error.to_string())?,
                HeaderValue::from_static(value),
            );
        }
        let (socket, _) =
            tungstenite::client(request, stream).map_err(|error| error.to_string())?;
        // From now on a read returns at once when nothing has arrived.
        socket
            .get_ref()
            .sock
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        Ok(Box::new(NativeSocket {
            socket,
            opened: false,
            closed: false,
        }))
    }
}

struct NativeSocket {
    socket: WebSocket<TlsStream>,
    opened: bool,
    closed: bool,
}

fn would_block(error: &tungstenite::Error) -> bool {
    matches!(error, tungstenite::Error::Io(io) if matches!(
        io.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ))
}

impl Socket for NativeSocket {
    fn poll(&mut self, out: &mut Vec<SocketEvent>) {
        if !self.opened {
            self.opened = true;
            out.push(SocketEvent::Open);
        }
        if self.closed {
            return;
        }
        loop {
            match self.socket.read() {
                Ok(WsMessage::Binary(bytes)) => out.push(SocketEvent::Data(bytes.to_vec())),
                Ok(WsMessage::Close(frame)) => {
                    self.closed = true;
                    let reason =
                        frame.map_or_else(|| "closed".into(), |frame| frame.reason.to_string());
                    out.push(SocketEvent::Closed(reason));
                    return;
                }
                // Pings are answered by tungstenite on the next write.
                Ok(_) => {}
                Err(error) if would_block(&error) => break,
                Err(error) => {
                    self.closed = true;
                    out.push(SocketEvent::Closed(error.to_string()));
                    return;
                }
            }
        }
        // Sends what is queued: pongs, and frames a full socket held back.
        if let Err(error) = self.socket.flush()
            && !would_block(&error)
        {
            self.closed = true;
            out.push(SocketEvent::Closed(error.to_string()));
        }
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        match self.socket.send(WsMessage::binary(bytes.to_vec())) {
            Ok(()) => Ok(()),
            // Queued; `poll` flushes it.
            Err(error) if would_block(&error) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

impl Drop for NativeSocket {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.socket.close(None);
            let _ = self.socket.flush();
        }
    }
}

/// One HTTPS GET of `url` with `headers`: the body, unzipped.
pub fn https_get(
    config: &Arc<ClientConfig>,
    url: &str,
    headers: &[(&str, &str)],
) -> Result<Vec<u8>, String> {
    let host = host_of(url).ok_or_else(|| format!("{url}: no host"))?;
    let path = url
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|ix| &rest[ix..]))
        .unwrap_or("/");
    let mut stream = connect_tls(config, host)?;
    let mut request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: application/json\r\nAccept-Encoding: gzip\r\n"
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("{url}: {error}"))?;
    let mut response = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                response.extend_from_slice(&buffer[..read]);
                if response.len() > MAX_RESPONSE {
                    return Err(format!("{url}: over {MAX_RESPONSE} bytes"));
                }
            }
            // A server that closes without TLS's goodbye: the body tells
            // whether it is whole.
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(format!("{url}: {error}")),
        }
    }
    let body = parse_response(&response).map_err(|error| format!("{url}: {error}"))?;
    Ok(body)
}

/// The body of an HTTP/1.1 response: status 200, de-chunked, unzipped.
pub fn parse_response(response: &[u8]) -> Result<Vec<u8>, String> {
    let end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("no end of the headers")?;
    let head = std::str::from_utf8(&response[..end]).map_err(|_| "headers not UTF-8")?;
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default();
    let code = status.split(' ').nth(1).unwrap_or_default();
    if code != "200" {
        return Err(format!("HTTP {status}"));
    }
    let header = |name: &str| {
        lines.clone().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_ascii_lowercase())
        })
    };
    let mut body = response[end + 4..].to_vec();
    if header("transfer-encoding").is_some_and(|value| value.contains("chunked")) {
        body = dechunk(&body)?;
    } else if let Some(length) = header("content-length").and_then(|value| value.parse().ok())
        && body.len() < length
    {
        return Err(format!("{} of {length} bytes", body.len()));
    }
    if header("content-encoding").is_some_and(|value| value.contains("gzip")) {
        let mut unzipped = Vec::new();
        flate2::read::GzDecoder::new(body.as_slice())
            .take(MAX_RESPONSE as u64)
            .read_to_end(&mut unzipped)
            .map_err(|error| format!("gzip: {error}"))?;
        body = unzipped;
    }
    Ok(body)
}

/// A chunked body, joined.
pub fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or("a chunk without its size")?;
        let size_text = std::str::from_utf8(&body[..line_end]).map_err(|_| "chunk size")?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| "chunk size")?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        let chunk = body.get(..size).ok_or("a chunk cut short")?;
        out.extend_from_slice(chunk);
        body = body.get(size + 2..).ok_or("a chunk cut short")?;
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn test_chunked_bodies_join() {
        let body = b"4\r\nWiki\r\n6;ext=1\r\npedia \r\nE\r\nin \r\n\r\nchunks.\r\n0\r\n\r\n";
        assert_eq!(
            dechunk(body).expect("whole"),
            b"Wikipedia in \r\n\r\nchunks."
        );
        assert!(dechunk(b"4\r\nWi").is_err());
        assert!(dechunk(b"zz\r\n").is_err());
    }

    #[test]
    fn test_responses_unzip_and_check_their_status() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(b"[1,2,3]").expect("gzip");
        let zipped = gz.finish().expect("gzip");
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
            zipped.len()
        )
        .into_bytes();
        response.extend(&zipped);
        assert_eq!(parse_response(&response).expect("a body"), b"[1,2,3]");
        // Cut short.
        assert!(parse_response(&response[..response.len() - 3]).is_err());
        assert!(parse_response(b"HTTP/1.1 429 Too Many Requests\r\n\r\n").is_err());
        let chunked = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n2\r\n[]\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked).expect("a body"), b"[]");
    }

    #[test]
    fn test_hosts_of_urls() {
        assert_eq!(
            host_of("wss://meri.digitraffic.fi/mqtt"),
            Some("meri.digitraffic.fi")
        );
        assert_eq!(host_of("wss://mqtt.hsl.fi:443/"), Some("mqtt.hsl.fi"));
        assert_eq!(
            host_of("https://meri.digitraffic.fi/api/ais/v1/vessels"),
            Some("meri.digitraffic.fi")
        );
        assert_eq!(host_of("no url"), None);
    }
}
