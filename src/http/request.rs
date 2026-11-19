use super::{Method, StatusCode};
use std::error::Error;
use std::fmt;
use std::io::{self, BufRead, Read};
use std::str;

/// Maximum number of header fields accepted in one request.
const MAX_HEADER_COUNT: usize = 100;
/// Maximum request-line size (in bytes, excluding CRLF).
const MAX_REQUEST_LINE_BYTES: usize = 8 * 1024;
/// Maximum size of one header line (in bytes, excluding CRLF).
const MAX_HEADER_LINE_BYTES: usize = 8 * 1024;

/// HTTP protocol versions supported by the server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Version {
    Http10,
    Http11,
}

impl Version {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http10 => "HTTP/1.0",
            Self::Http11 => "HTTP/1.1",
        }
    }
}

/// A case-insensitive, insertion-ordered collection of request headers.
///
/// Header names are stored in lowercase. Values are kept as received so that
/// duplicate headers such as `Connection` can be inspected separately.
#[derive(Clone, Debug, Default)]
pub struct Headers {
    entries: Vec<(String, String)>,
}

impl Headers {
    fn push(&mut self, name: String, value: String) {
        self.entries.push((name, value));
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.entries
            .iter()
            .filter(move |(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Check a comma-separated header (e.g. `Connection`) for a token.
    pub fn contains_token(&self, name: &str, token: &str) -> bool {
        self.get_all(name)
            .flat_map(|value| value.split(','))
            .any(|value| value.trim().eq_ignore_ascii_case(token))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A parsed HTTP request.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: Method,
    /// Raw request target, including the query string.
    pub target: String,
    /// Raw path (still percent-encoded).
    pub path: String,
    /// Raw query string without the leading `?`.
    pub query: Option<String>,
    pub version: Version,
    pub headers: Headers,
    pub body: Vec<u8>,
    /// Whether the connection may be reused after this request.
    pub keep_alive: bool,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)
    }

    pub fn body_text(&self) -> Result<&str, str::Utf8Error> {
        str::from_utf8(&self.body)
    }
}

/// Errors that can occur while reading a request.
///
/// Socket errors close the connection. Protocol errors are mapped to a final
/// HTTP response and also close the connection, because the parser may no
/// longer know where the next request begins.
#[derive(Debug)]
pub enum RequestError {
    Io(io::Error),
    BadRequest(&'static str),
    UriTooLong,
    HeadersTooLarge,
    BodyTooLarge,
    UnsupportedMethod,
    UnsupportedTransferEncoding,
    ExpectationFailed,
    VersionNotSupported,
}

impl RequestError {
    pub fn client_status(&self) -> Option<StatusCode> {
        match self {
            Self::Io(_) => None,
            Self::BadRequest(_) => Some(StatusCode::BadRequest),
            Self::UriTooLong => Some(StatusCode::UriTooLong),
            Self::HeadersTooLarge => Some(StatusCode::RequestHeaderFieldsTooLarge),
            Self::BodyTooLarge => Some(StatusCode::PayloadTooLarge),
            Self::UnsupportedMethod | Self::UnsupportedTransferEncoding => {
                Some(StatusCode::NotImplemented)
            }
            Self::ExpectationFailed => Some(StatusCode::ExpectationFailed),
            Self::VersionNotSupported => Some(StatusCode::HttpVersionNotSupported),
        }
    }
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::BadRequest(message) => write!(f, "bad request: {message}"),
            Self::UriTooLong => f.write_str("request target is too long"),
            Self::HeadersTooLarge => f.write_str("request headers are too large"),
            Self::BodyTooLarge => f.write_str("request body is too large"),
            Self::UnsupportedMethod => f.write_str("unsupported HTTP method"),
            Self::UnsupportedTransferEncoding => f.write_str("Transfer-Encoding is not supported"),
            Self::ExpectationFailed => f.write_str("unsupported Expect header"),
            Self::VersionNotSupported => f.write_str("unsupported HTTP version"),
        }
    }
}

impl Error for RequestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for RequestError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Read and parse exactly one request from `reader`.
///
/// Returns `Ok(None)` only for a clean EOF before a request starts. The body
/// is read according to `Content-Length`; chunked request bodies are rejected.
///
/// If the request contains `Expect: 100-continue`, `on_continue` is called
/// after the headers have been validated and before the body is read. The
/// server uses that callback to write the interim response. Callers that do
/// not need this behavior can pass `|| Ok(())`.
pub fn read_request<R: BufRead, F: FnOnce() -> io::Result<()>>(
    reader: &mut R,
    max_header_bytes: usize,
    max_body_bytes: usize,
    on_continue: F,
) -> Result<Option<Request>, RequestError> {
    let request_line_limit = MAX_REQUEST_LINE_BYTES.min(max_header_bytes.max(1));
    let request_line_bytes = match read_line(reader, request_line_limit) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(LineError::Io(error)) => return Err(RequestError::Io(error)),
        Err(LineError::TooLong) => return Err(RequestError::UriTooLong),
        Err(LineError::Unterminated) => {
            return Err(RequestError::BadRequest("unterminated request line"))
        }
        Err(LineError::BareCr) => return Err(RequestError::BadRequest("bare CR in request line")),
    };

    let mut header_bytes = request_line_bytes.len() + 2;
    if header_bytes > max_header_bytes {
        return Err(RequestError::HeadersTooLarge);
    }

    let request_line = str::from_utf8(&request_line_bytes)
        .map_err(|_| RequestError::BadRequest("request line is not valid UTF-8"))?;
    let mut parts = request_line.split(' ');
    let method_token = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or(RequestError::BadRequest("missing method"))?;
    let target = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or(RequestError::BadRequest("missing request target"))?;
    let version_token = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or(RequestError::BadRequest("missing HTTP version"))?;
    if parts.next().is_some() {
        return Err(RequestError::BadRequest(
            "request line must contain exactly three fields",
        ));
    }

    let method = match Method::from_token(method_token) {
        Some(method) => method,
        None if is_token(method_token) => return Err(RequestError::UnsupportedMethod),
        None => return Err(RequestError::BadRequest("invalid method token")),
    };

    let version = match version_token {
        "HTTP/1.1" => Version::Http11,
        "HTTP/1.0" => Version::Http10,
        version if version.starts_with("HTTP/") => return Err(RequestError::VersionNotSupported),
        _ => return Err(RequestError::BadRequest("invalid HTTP version")),
    };

    if !target.starts_with('/') {
        return Err(RequestError::BadRequest(
            "only origin-form request targets are supported",
        ));
    }
    if target.contains('#') {
        return Err(RequestError::BadRequest(
            "request target must not contain a fragment",
        ));
    }
    if target
        .bytes()
        .any(|byte| byte <= b' ' || byte == 0x7f || byte == b'\\')
    {
        return Err(RequestError::BadRequest(
            "request target contains invalid characters",
        ));
    }

    let mut headers = Headers::default();
    loop {
        if header_bytes >= max_header_bytes {
            return Err(RequestError::HeadersTooLarge);
        }

        let remaining = max_header_bytes - header_bytes;
        let line_limit = MAX_HEADER_LINE_BYTES.min(remaining);
        let line_bytes = match read_line(reader, line_limit) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Err(RequestError::BadRequest("unexpected EOF in headers")),
            Err(LineError::Io(error)) => return Err(RequestError::Io(error)),
            Err(LineError::TooLong) => return Err(RequestError::HeadersTooLarge),
            Err(LineError::Unterminated) => {
                return Err(RequestError::BadRequest("unterminated header line"))
            }
            Err(LineError::BareCr) => {
                return Err(RequestError::BadRequest("bare CR in header line"))
            }
        };
        header_bytes += line_bytes.len() + 2;
        if header_bytes > max_header_bytes {
            return Err(RequestError::HeadersTooLarge);
        }

        if line_bytes.is_empty() {
            break;
        }

        if headers.len() >= MAX_HEADER_COUNT {
            return Err(RequestError::HeadersTooLarge);
        }

        let line = str::from_utf8(&line_bytes)
            .map_err(|_| RequestError::BadRequest("header line is not valid UTF-8"))?;
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err(RequestError::BadRequest(
                "obsolete header line folding is not supported",
            ));
        }

        let (name, value) = line
            .split_once(':')
            .ok_or(RequestError::BadRequest("header line is missing ':'"))?;
        if name.is_empty() || !name.bytes().all(is_tchar) {
            return Err(RequestError::BadRequest("invalid header name"));
        }
        let value = value.trim_matches(|character| character == ' ' || character == '\t');
        if value.chars().any(|character| {
            character == '\r' || character == '\n' || (character.is_control() && character != '\t')
        }) {
            return Err(RequestError::BadRequest("invalid header value"));
        }

        headers.push(name.to_ascii_lowercase(), value.to_string());
    }

    let host_count = headers.get_all("host").count();
    if host_count > 1 {
        return Err(RequestError::BadRequest("multiple Host headers"));
    }
    if version == Version::Http11 && host_count != 1 {
        return Err(RequestError::BadRequest(
            "HTTP/1.1 requires exactly one Host header",
        ));
    }
    if let Some(host) = headers.get("host") {
        if host.is_empty()
            || host.contains(',')
            || host.bytes().any(|byte| byte <= b' ' || byte == 0x7f)
        {
            return Err(RequestError::BadRequest("invalid Host header"));
        }
    }

    let expect_values: Vec<&str> = headers.get_all("expect").collect();
    let expect_continue = match expect_values.as_slice() {
        [] => false,
        [value] if version == Version::Http11 && value.eq_ignore_ascii_case("100-continue") => true,
        _ => return Err(RequestError::ExpectationFailed),
    };

    let content_lengths: Vec<&str> = headers.get_all("content-length").collect();
    let has_transfer_encoding = headers.get("transfer-encoding").is_some();
    if has_transfer_encoding {
        if !content_lengths.is_empty() {
            return Err(RequestError::BadRequest(
                "Transfer-Encoding and Content-Length cannot be combined",
            ));
        }
        return Err(RequestError::UnsupportedTransferEncoding);
    }
    if content_lengths.len() > 1 {
        return Err(RequestError::BadRequest(
            "multiple Content-Length headers are not allowed",
        ));
    }

    let content_length = match content_lengths.first() {
        Some(value) if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
            value
                .parse::<u64>()
                .map_err(|_| RequestError::BodyTooLarge)?
        }
        Some(_) => return Err(RequestError::BadRequest("invalid Content-Length")),
        None => 0,
    };
    if content_length > max_body_bytes as u64 {
        return Err(RequestError::BodyTooLarge);
    }

    if expect_continue {
        on_continue().map_err(RequestError::Io)?;
    }

    let mut body = vec![0; content_length as usize];
    if let Err(error) = reader.read_exact(&mut body) {
        return Err(if error.kind() == io::ErrorKind::UnexpectedEof {
            RequestError::BadRequest("incomplete request body")
        } else {
            RequestError::Io(error)
        });
    }

    let mut keep_alive = version == Version::Http11;
    if headers.contains_token("connection", "close") {
        keep_alive = false;
    } else if version == Version::Http10 && headers.contains_token("connection", "keep-alive") {
        keep_alive = true;
    }

    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), Some(query.to_string())),
        None => (target.to_string(), None),
    };
    if path.is_empty() {
        return Err(RequestError::BadRequest("empty request path"));
    }

    Ok(Some(Request {
        method,
        target: target.to_string(),
        path,
        query,
        version,
        headers,
        body,
        keep_alive,
    }))
}

enum LineError {
    Io(io::Error),
    TooLong,
    Unterminated,
    BareCr,
}

/// Read one CRLF/LF-terminated line without allowing unbounded allocation.
fn read_line<R: BufRead>(reader: &mut R, limit: usize) -> Result<Option<Vec<u8>>, LineError> {
    let mut line = Vec::new();
    let read = reader
        .by_ref()
        .take(limit.saturating_add(2) as u64)
        .read_until(b'\n', &mut line)
        .map_err(LineError::Io)?;

    if read == 0 {
        return Ok(None);
    }
    if !line.ends_with(b"\n") {
        return Err(if line.len() > limit {
            LineError::TooLong
        } else {
            LineError::Unterminated
        });
    }

    line.pop();
    if line.ends_with(b"\r") {
        line.pop();
    }
    if line.len() > limit {
        return Err(LineError::TooLong);
    }
    if line.contains(&b'\r') {
        return Err(LineError::BareCr);
    }

    Ok(Some(line))
}

fn is_token(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(is_tchar)
}

fn is_tchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(raw: &str) -> Result<Option<Request>, RequestError> {
        read_request(&mut Cursor::new(raw.as_bytes()), 16 * 1024, 1024, || Ok(()))
    }

    #[test]
    fn parses_request_line_headers_and_query() {
        let request = parse(
            "GET /hello?name=Ada HTTP/1.1\r\n\
             Host: example.test\r\n\
             User-Agent: tests\r\n\
             Connection: keep-alive\r\n\r\n",
        )
        .unwrap()
        .unwrap();

        assert_eq!(request.method, Method::Get);
        assert_eq!(request.path, "/hello");
        assert_eq!(request.query.as_deref(), Some("name=Ada"));
        assert_eq!(request.version, Version::Http11);
        assert_eq!(request.header("USER-AGENT"), Some("tests"));
        assert!(request.keep_alive);
        assert!(request.body.is_empty());
    }

    #[test]
    fn parses_content_length_body() {
        let request = parse("POST /api/echo HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello")
            .unwrap()
            .unwrap();

        assert_eq!(request.method, Method::Post);
        assert_eq!(request.body_text().unwrap(), "hello");
    }

    #[test]
    fn handles_expect_100_continue() {
        let raw = "POST /api/echo HTTP/1.1\r\n\
                   Host: x\r\n\
                   Content-Length: 5\r\n\
                   Expect: 100-Continue\r\n\r\n\
                   hello";
        let mut called = false;
        let request = read_request(&mut Cursor::new(raw.as_bytes()), 16 * 1024, 1024, || {
            called = true;
            Ok(())
        })
        .unwrap()
        .unwrap();

        assert!(called);
        assert_eq!(request.body_text().unwrap(), "hello");

        let mut unsupported_called = false;
        let error = read_request(
            &mut Cursor::new(
                b"POST / HTTP/1.1\r\nHost: x\r\nExpect: magic\r\nContent-Length: 1\r\n\r\nx",
            ),
            16 * 1024,
            1024,
            || {
                unsupported_called = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(matches!(error, RequestError::ExpectationFailed));
        assert!(!unsupported_called);
    }

    #[test]
    fn applies_http_10_keep_alive_rules() {
        let close = parse("GET / HTTP/1.0\r\n\r\n").unwrap().unwrap();
        assert!(!close.keep_alive);

        let keep = parse("GET / HTTP/1.0\r\nConnection: keep-alive\r\n\r\n")
            .unwrap()
            .unwrap();
        assert!(keep.keep_alive);

        let close_wins =
            parse("GET / HTTP/1.1\r\nHost: x\r\nConnection: keep-alive, close\r\n\r\n")
                .unwrap()
                .unwrap();
        assert!(!close_wins.keep_alive);
    }

    #[test]
    fn rejects_ambiguous_or_unsafe_requests() {
        assert!(matches!(
            parse("GET / HTTP/1.1\r\n\r\n"),
            Err(RequestError::BadRequest(_))
        ));
        assert!(matches!(
            parse("POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx"),
            Err(RequestError::BadRequest(_))
        ));
        assert!(matches!(
            parse(
                "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\nx"
            ),
            Err(RequestError::BadRequest(_))
        ));
        assert!(matches!(
            parse("POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Err(RequestError::UnsupportedTransferEncoding)
        ));
        assert!(matches!(
            parse("FETCH / HTTP/1.1\r\nHost: x\r\n\r\n"),
            Err(RequestError::UnsupportedMethod)
        ));
        assert!(matches!(
            parse("GET / HTTP/2.0\r\nHost: x\r\n\r\n"),
            Err(RequestError::VersionNotSupported)
        ));
        assert!(matches!(
            parse("GET http://x/ HTTP/1.1\r\nHost: x\r\n\r\n"),
            Err(RequestError::BadRequest(_))
        ));
    }

    #[test]
    fn enforces_body_and_header_limits() {
        let oversized_body = "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 2048\r\n\r\n";
        assert!(matches!(
            parse(oversized_body),
            Err(RequestError::BodyTooLarge)
        ));

        let oversized_headers =
            "GET / HTTP/1.1\r\nHost: x\r\nX-Long: 0123456789012345678901234567890123456789\r\n\r\n";
        assert!(matches!(
            read_request(
                &mut Cursor::new(oversized_headers.as_bytes()),
                48,
                1024,
                || Ok(())
            ),
            Err(RequestError::HeadersTooLarge)
        ));
    }

    #[test]
    fn clean_eof_is_not_an_error() {
        assert!(parse("").unwrap().is_none());
        assert!(matches!(
            parse("GET / HTTP/1.1\r\nHost: x"),
            Err(RequestError::BadRequest(_))
        ));
    }
}
