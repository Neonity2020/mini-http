//! A small, dependency-free HTTP/1.1 server used as a teaching project.
//!
//! The implementation deliberately stops at the parts that make HTTP
//! understandable: TCP, request parsing, responses, routing, keep-alive and
//! concurrency. TLS, HTTP/2, chunked transfer encoding and async I/O are left
//! as exercises. See `TUTORIAL.md` for a guided tour.

pub mod app;
pub mod http;
pub mod router;
pub mod server;
