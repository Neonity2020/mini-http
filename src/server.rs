use crate::http::{read_request, Method, RequestError, Response, StatusCode, Version};
use crate::router::Router;
use std::io::{self, BufReader, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Runtime limits applied to every connection.
///
/// The defaults are intentionally conservative: this is a teaching server, so
/// bounded inputs matter more than maximizing throughput.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Maximum bytes for the request line plus all headers.
    pub max_header_bytes: usize,
    /// Maximum `Content-Length` accepted for a request body.
    pub max_body_bytes: usize,
    /// Requests allowed on one keep-alive connection.
    pub max_requests_per_connection: usize,
    /// Simultaneous TCP connections before the server starts returning 503.
    pub max_connections: usize,
    /// Socket read timeout, including idle keep-alive time.
    pub read_timeout: Duration,
    /// Socket write timeout.
    pub write_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_header_bytes: 16 * 1024,
            max_body_bytes: 1024 * 1024,
            max_requests_per_connection: 100,
            max_connections: 256,
            read_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(10),
        }
    }
}

/// A blocking, thread-per-connection HTTP server.
pub struct Server {
    listener: TcpListener,
    router: Router,
    config: ServerConfig,
}

impl Server {
    pub fn bind(
        address: impl ToSocketAddrs,
        router: Router,
        config: ServerConfig,
    ) -> io::Result<Self> {
        Ok(Self {
            listener: TcpListener::bind(address)?,
            router,
            config,
        })
    }

    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Accept connections forever. Each connection gets its own thread.
    pub fn run(self) -> io::Result<()> {
        let router = Arc::new(self.router);
        let config = self.config;
        let active = Arc::new(AtomicUsize::new(0));

        loop {
            let (stream, peer) = match self.listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };

            let previous = active.fetch_add(1, Ordering::Relaxed);
            if previous >= config.max_connections {
                active.fetch_sub(1, Ordering::Relaxed);
                reject_overloaded(stream);
                continue;
            }

            let guard = ConnectionGuard(Arc::clone(&active));
            let router = Arc::clone(&router);
            let config = config.clone();
            let thread_name = format!("mini-http-{peer}");

            let spawned = thread::Builder::new().name(thread_name).spawn(move || {
                let _guard = guard;
                serve_connection(stream, router, config);
            });

            if let Err(error) = spawned {
                eprintln!("failed to spawn connection thread for {peer}: {error}");
            }
        }
    }
}

/// Serve requests on one accepted connection.
///
/// This function is public so integration tests and examples can drive the
/// connection loop without starting the accept loop. Applications should use
/// [`Server::run`] instead.
pub fn serve_connection(stream: TcpStream, router: Arc<Router>, config: ServerConfig) {
    let peer = stream
        .peer_addr()
        .map(|address| address.to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(config.read_timeout));
    let _ = stream.set_write_timeout(Some(config.write_timeout));

    let read_half = match stream.try_clone() {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("{peer} could not clone socket: {error}");
            return;
        }
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    let mut served = 0usize;

    loop {
        let request = match read_request(
            &mut reader,
            config.max_header_bytes,
            config.max_body_bytes,
            || {
                writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
                writer.flush()
            },
        ) {
            Ok(Some(request)) => request,
            Ok(None) => break,
            Err(RequestError::Io(error)) => {
                if served == 0
                    && matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    )
                {
                    let response = Response::text(
                        StatusCode::RequestTimeout,
                        "timed out while reading request\n",
                    );
                    let _ = response.write_http(&mut writer, Version::Http11, false, false);
                }
                break;
            }
            Err(error) => {
                eprintln!("{peer} rejected request: {error}");
                if let Some(status) = error.client_status() {
                    let response = Response::text(status, format!("{}\n", status.reason()));
                    let _ = response.write_http(&mut writer, Version::Http11, false, false);
                }
                break;
            }
        };

        served += 1;
        let started = Instant::now();
        let response = router.dispatch(&request);
        let keep_alive = request.keep_alive && served < config.max_requests_per_connection;
        let status = response.status().code();
        let body_bytes = response.body_len();
        let head_request = request.method == Method::Head;

        if let Err(error) =
            response.write_http(&mut writer, request.version, head_request, keep_alive)
        {
            eprintln!("{peer} failed to write response: {error}");
            break;
        }

        println!(
            "{peer} \"{} {}\" -> {} ({} bytes, {}us)",
            request.method,
            request.target,
            status,
            body_bytes,
            started.elapsed().as_micros()
        );

        if !keep_alive {
            break;
        }
    }
}

struct ConnectionGuard(Arc<AtomicUsize>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn reject_overloaded(mut stream: TcpStream) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let response = Response::text(
        StatusCode::ServiceUnavailable,
        "server is at its connection limit\n",
    );
    let _ = response.write_http(&mut stream, Version::Http11, false, false);
}
