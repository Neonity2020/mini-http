use super::{StatusCode, Version};
use std::fmt::Write as _;
use std::io::{self, Write};

const SERVER_NAME: &str = "mini-http/0.1";

/// A response ready to be written to a connection.
#[derive(Clone, Debug)]
pub struct Response {
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    pub fn new(status: StatusCode) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn text(status: StatusCode, body: impl Into<String>) -> Self {
        Self::bytes(
            status,
            "text/plain; charset=utf-8",
            body.into().into_bytes(),
        )
    }

    pub fn html(status: StatusCode, body: impl Into<String>) -> Self {
        Self::bytes(status, "text/html; charset=utf-8", body.into().into_bytes())
    }

    pub fn json(status: StatusCode, body: impl Into<String>) -> Self {
        Self::bytes(
            status,
            "application/json; charset=utf-8",
            body.into().into_bytes(),
        )
    }

    pub fn bytes(status: StatusCode, content_type: impl Into<String>, body: Vec<u8>) -> Self {
        let mut response = Self::new(status).with_header("Content-Type", content_type);
        response.body = body;
        response
    }

    /// Create a bodyless response. The connection frame still supplies
    /// `Content-Length: 0` for statuses that permit a body.
    pub fn empty(status: StatusCode) -> Self {
        Self::new(status)
    }

    /// Add or replace a response header.
    ///
    /// # Panics
    ///
    /// Panics on invalid field names/values or on an attempt to set headers
    /// owned by the HTTP framing layer. Failing fast at the call site is much
    /// safer than allowing CRLF injection or a conflicting `Content-Length`
    /// onto the wire.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let name = name.into();
        let value = value.into();

        assert!(is_token(&name), "invalid response header name: {name:?}");
        assert!(
            is_valid_header_value(&value),
            "invalid response header value for {name}"
        );

        let lower_name = name.to_ascii_lowercase();
        assert!(
            !matches!(
                lower_name.as_str(),
                "content-length" | "connection" | "transfer-encoding" | "server"
            ),
            "header {name} is managed by the server"
        );

        self.headers
            .retain(|(existing, _)| !existing.eq_ignore_ascii_case(&name));
        self.headers.push((name, value));
        self
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn body_len(&self) -> usize {
        self.body.len()
    }

    /// Serialize the response and write it to `writer`.
    ///
    /// `head_request` suppresses the body while preserving the length that a
    /// `GET` would have returned. `keep_alive` controls the `Connection`
    /// header; the caller still owns the connection loop.
    pub fn write_http<W: Write>(
        &self,
        writer: &mut W,
        version: Version,
        head_request: bool,
        keep_alive: bool,
    ) -> io::Result<()> {
        let allows_body = self.status.allows_body();
        let mut head = String::with_capacity(256);

        write!(
            head,
            "{} {} {}\r\n",
            version.as_str(),
            self.status.code(),
            self.status.reason()
        )
        .expect("writing to a String cannot fail");
        writeln!(head, "Server: {SERVER_NAME}\r").expect("writing to a String cannot fail");
        if allows_body {
            writeln!(head, "Content-Length: {}\r", self.body.len())
                .expect("writing to a String cannot fail");
        }
        for (name, value) in &self.headers {
            write!(head, "{name}: {value}\r\n").expect("writing to a String cannot fail");
        }
        if keep_alive {
            head.push_str("Connection: keep-alive\r\n");
        } else {
            head.push_str("Connection: close\r\n");
        }
        head.push_str("\r\n");

        writer.write_all(head.as_bytes())?;
        if allows_body && !head_request {
            writer.write_all(&self.body)?;
        }
        writer.flush()
    }

    /// Convenience wrapper used by tests and examples.
    pub fn to_http_bytes(&self, version: Version, head_request: bool, keep_alive: bool) -> Vec<u8> {
        let mut output = Vec::new();
        self.write_http(&mut output, version, head_request, keep_alive)
            .expect("writing to a Vec cannot fail");
        output
    }
}

fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn is_valid_header_value(value: &str) -> bool {
    !value.chars().any(|character| {
        character == '\r'
            || character == '\n'
            || character == '\0'
            || (character.is_control() && character != '\t')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn serializes_content_length_and_connection() {
        let response = Response::text(StatusCode::Ok, "hello");
        let wire = text(&response.to_http_bytes(Version::Http11, false, true));

        assert!(wire.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(wire.contains("Content-Length: 5\r\n"));
        assert!(wire.contains("Connection: keep-alive\r\n"));
        assert!(wire.ends_with("\r\n\r\nhello"));
    }

    #[test]
    fn head_preserves_length_but_omits_body() {
        let response = Response::text(StatusCode::Ok, "hello");
        let wire = text(&response.to_http_bytes(Version::Http11, true, false));

        assert!(wire.contains("Content-Length: 5\r\n"));
        assert!(wire.ends_with("\r\n\r\n"));
    }

    #[test]
    fn no_content_has_no_body_or_length() {
        let response = Response::empty(StatusCode::NoContent);
        let wire = text(&response.to_http_bytes(Version::Http11, false, false));

        assert!(!wire.contains("Content-Length:"));
        assert!(wire.ends_with("\r\n\r\n"));
    }

    #[test]
    #[should_panic(expected = "invalid response header value")]
    fn rejects_header_injection() {
        let _ =
            Response::text(StatusCode::Ok, "x").with_header("X-Test", "value\r\nX-Injected: yes");
    }

    #[test]
    #[should_panic(expected = "managed by the server")]
    fn rejects_framing_headers() {
        let _ = Response::text(StatusCode::Ok, "x").with_header("Content-Length", "999");
    }
}
