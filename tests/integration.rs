use mini_http::app;
use mini_http::server::{serve_connection, ServerConfig};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

struct TestServer {
    address: SocketAddr,
    handle: JoinHandle<()>,
}

impl TestServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let address = listener.local_addr().expect("test listener address");
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept test connection");
            serve_connection(stream, Arc::new(app::router()), ServerConfig::default());
        });
        Self { address, handle }
    }

    fn connect(&self) -> Client {
        Client::new(self.address)
    }

    fn join(self) {
        self.handle.join().expect("server thread");
    }
}

struct Client {
    reader: BufReader<TcpStream>,
}

impl Client {
    fn new(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).expect("connect to test server");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("set write timeout");
        Self {
            reader: BufReader::new(stream),
        }
    }

    fn send(&mut self, request: &str) {
        self.reader
            .get_mut()
            .write_all(request.as_bytes())
            .expect("write request");
        self.reader.get_mut().flush().expect("flush request");
    }

    fn read_continue(&mut self) {
        let mut status_line = String::new();
        self.reader
            .read_line(&mut status_line)
            .expect("read interim status");
        assert_eq!(status_line, "HTTP/1.1 100 Continue\r\n");

        let mut blank_line = String::new();
        self.reader
            .read_line(&mut blank_line)
            .expect("read interim end");
        assert_eq!(blank_line, "\r\n");
    }

    fn read_response(&mut self, read_body: bool) -> HttpResponse {
        let mut status_line = String::new();
        self.reader
            .read_line(&mut status_line)
            .expect("read status line");
        assert!(
            status_line.starts_with("HTTP/1."),
            "unexpected status line: {status_line:?}"
        );
        let status = status_line
            .split_whitespace()
            .nth(1)
            .expect("status code")
            .parse()
            .expect("numeric status code");

        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).expect("read header");
            if line == "\r\n" || line == "\n" {
                break;
            }
            let (name, value) = line.split_once(':').expect("header colon");
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }

        let content_length = headers
            .iter()
            .find(|(name, _)| name == "content-length")
            .map(|(_, value)| value.parse::<usize>().expect("content length"))
            .unwrap_or(0);

        let mut body = Vec::new();
        if read_body {
            body.resize(content_length, 0);
            self.reader
                .read_exact(&mut body)
                .expect("read response body");
        }

        HttpResponse {
            status,
            headers,
            body,
        }
    }

    fn assert_eof(&mut self) {
        let mut extra = Vec::new();
        self.reader
            .read_to_end(&mut extra)
            .expect("read until server closes");
        assert!(
            extra.is_empty(),
            "unexpected bytes after response: {extra:?}"
        );
    }
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }
}

#[test]
fn serves_multiple_requests_on_one_keep_alive_connection() {
    let server = TestServer::start();
    let mut client = server.connect();

    client.send("GET /api/health HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let health = client.read_response(true);
    assert_eq!(health.status, 200);
    assert_eq!(health.header("connection"), Some("keep-alive"));
    assert!(String::from_utf8_lossy(&health.body).contains("\"status\":\"ok\""));

    client.send(
        "POST /api/echo HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: text/plain\r\n\
         Content-Length: 5\r\n\r\n\
         hello",
    );
    let echo = client.read_response(true);
    assert_eq!(echo.status, 200);
    assert_eq!(echo.body, b"hello");
    assert_eq!(echo.header("content-type"), Some("text/plain"));
    assert_eq!(echo.header("x-body-bytes"), Some("5"));

    client.send("GET /api/users/42 HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let user = client.read_response(true);
    assert_eq!(user.status, 200);
    assert!(String::from_utf8_lossy(&user.body).contains("\"name\":\"user-42\""));

    client.send(
        "GET /missing HTTP/1.1\r\n\
         Host: localhost\r\n\
         Connection: close\r\n\r\n",
    );
    let missing = client.read_response(true);
    assert_eq!(missing.status, 404);
    assert_eq!(missing.header("connection"), Some("close"));
    client.assert_eof();

    server.join();
}

#[test]
fn head_has_headers_but_no_body_and_405_lists_allowed_methods() {
    let server = TestServer::start();
    let mut client = server.connect();

    client.send("POST /api/health HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let wrong_method = client.read_response(true);
    assert_eq!(wrong_method.status, 405);
    assert_eq!(wrong_method.header("allow"), Some("GET, HEAD"));

    client.send(
        "HEAD /api/health HTTP/1.1\r\n\
         Host: localhost\r\n\
         Connection: close\r\n\r\n",
    );
    let head = client.read_response(false);
    assert_eq!(head.status, 200);
    assert!(head
        .header("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > 0));
    client.assert_eof();

    server.join();
}

#[test]
fn rejects_ambiguous_framing_and_closes_the_connection() {
    let server = TestServer::start();
    let mut client = server.connect();

    client.send(
        "POST /api/echo HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Length: 5\r\n\
         Transfer-Encoding: chunked\r\n\r\n\
         hello",
    );
    let response = client.read_response(true);
    assert_eq!(response.status, 400);
    assert_eq!(response.header("connection"), Some("close"));
    client.assert_eof();

    server.join();
}

#[test]
fn sends_an_interim_response_for_expect_100_continue() {
    let server = TestServer::start();
    let mut client = server.connect();

    client.send(
        "POST /api/echo HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Length: 5\r\n\
         Expect: 100-continue\r\n\
         Connection: close\r\n\r\n",
    );
    client.read_continue();
    client.send("hello");

    let response = client.read_response(true);
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"hello");
    client.assert_eof();

    server.join();
}
