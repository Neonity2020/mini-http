use mini_http::app;
use mini_http::server::{Server, ServerConfig};
use std::env;
use std::process::ExitCode;

const DEFAULT_ADDRESS: &str = "127.0.0.1:7878";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("mini-http: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let first = args.next();

    if matches!(first.as_deref(), Some("-h") | Some("--help")) {
        print_help();
        return Ok(());
    }
    if args.next().is_some() {
        return Err("expected at most one listen address (try --help)".to_string());
    }

    let address = first
        .or_else(|| env::var("MINI_HTTP_ADDR").ok())
        .unwrap_or_else(|| DEFAULT_ADDRESS.to_string());

    if address.starts_with('-') {
        return Err(format!("unknown option {address} (try --help)"));
    }

    let server = Server::bind(address.as_str(), app::router(), ServerConfig::default())
        .map_err(|error| format!("cannot bind {address}: {error}"))?;
    let local = server
        .local_addr()
        .map_err(|error| format!("cannot read local address: {error}"))?;

    println!("mini-http listening on http://{local}");
    println!("  GET  /                     HTML demo page");
    println!("  GET  /hello?name=Ada       query parameter demo");
    println!("  GET  /api/health           JSON health check");
    println!("  GET  /api/users/:id        path parameter demo");
    println!("  POST /api/echo             request body echo");
    println!("Press Ctrl+C to stop.");

    server
        .run()
        .map_err(|error| format!("server error: {error}"))
}

fn print_help() {
    println!(
        "mini-http 0.1 — a dependency-free HTTP/1.1 teaching server\n\
         \n\
         USAGE:\n\
         \x20 mini-http [ADDRESS]\n\
         \n\
         ADDRESS defaults to {DEFAULT_ADDRESS}, or to $MINI_HTTP_ADDR.\n\
         Use 127.0.0.1:0 to pick an available port automatically.\n\
         \n\
         EXAMPLES:\n\
         \x20 cargo run\n\
         \x20 cargo run -- 0.0.0.0:3000\n\
         \x20 MINI_HTTP_ADDR=127.0.0.1:9000 cargo run\n"
    );
}
