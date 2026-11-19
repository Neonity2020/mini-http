use crate::http::{percent_decode, DecodeError, Method, Request, Response, StatusCode};
use std::collections::HashSet;

/// Handlers are plain functions. They receive the parsed request and any path
/// parameters, and return a complete response.
pub type Handler = fn(&Request, &Params) -> Response;

/// Path parameters captured from a route such as `/users/:id`.
#[derive(Clone, Debug, Default)]
pub struct Params {
    values: Vec<(String, String)>,
}

impl Params {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

#[derive(Clone, Debug)]
enum Segment {
    Static(String),
    Param(String),
}

struct Route {
    method: Method,
    segments: Vec<Segment>,
    handler: Handler,
}

/// A small method + path router with first-match semantics.
///
/// `GET` routes also serve `HEAD` automatically; the response layer removes
/// the body while keeping the correct `Content-Length`.
pub struct Router {
    routes: Vec<Route>,
}

impl Router {
    pub fn new() -> Self {
        Self { routes: Vec::new() }
    }

    pub fn route(mut self, method: Method, pattern: &str, handler: Handler) -> Self {
        self.routes.push(Route {
            method,
            segments: parse_pattern(pattern),
            handler,
        });
        self
    }

    pub fn get(self, pattern: &str, handler: Handler) -> Self {
        self.route(Method::Get, pattern, handler)
    }

    pub fn head(self, pattern: &str, handler: Handler) -> Self {
        self.route(Method::Head, pattern, handler)
    }

    pub fn post(self, pattern: &str, handler: Handler) -> Self {
        self.route(Method::Post, pattern, handler)
    }

    pub fn put(self, pattern: &str, handler: Handler) -> Self {
        self.route(Method::Put, pattern, handler)
    }

    pub fn patch(self, pattern: &str, handler: Handler) -> Self {
        self.route(Method::Patch, pattern, handler)
    }

    pub fn delete(self, pattern: &str, handler: Handler) -> Self {
        self.route(Method::Delete, pattern, handler)
    }

    pub fn options(self, pattern: &str, handler: Handler) -> Self {
        self.route(Method::Options, pattern, handler)
    }

    pub fn dispatch(&self, request: &Request) -> Response {
        let path_segments = match decode_path(&request.path) {
            Ok(segments) => segments,
            Err(_) => {
                return Response::text(StatusCode::BadRequest, "invalid percent-encoding in path\n")
            }
        };

        let mut path_matched = false;
        let mut allowed = Vec::new();
        let mut head_fallback: Option<(&Route, Params)> = None;

        for route in &self.routes {
            let Some(params) = route.matches(&path_segments) else {
                continue;
            };

            path_matched = true;
            if !allowed.contains(&route.method) {
                allowed.push(route.method);
            }
            // HEAD is implicitly available wherever GET is.
            if route.method == Method::Get && !allowed.contains(&Method::Head) {
                allowed.push(Method::Head);
            }

            if route.method == request.method {
                return (route.handler)(request, &params);
            }
            if request.method == Method::Head
                && route.method == Method::Get
                && head_fallback.is_none()
            {
                head_fallback = Some((route, params));
            }
        }

        if let Some((route, params)) = head_fallback {
            return (route.handler)(request, &params);
        }

        if path_matched {
            return Response::text(StatusCode::MethodNotAllowed, "405 Method Not Allowed\n")
                .with_header("Allow", allow_header(&allowed));
        }

        Response::text(StatusCode::NotFound, "404 Not Found\n")
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Route {
    fn matches(&self, path: &[String]) -> Option<Params> {
        if self.segments.len() != path.len() {
            return None;
        }

        let mut params = Params::default();
        for (segment, value) in self.segments.iter().zip(path) {
            match segment {
                Segment::Static(expected) if expected == value => {}
                Segment::Static(_) => return None,
                Segment::Param(name) => {
                    if value.is_empty() {
                        return None;
                    }
                    params.values.push((name.clone(), value.clone()));
                }
            }
        }
        Some(params)
    }
}

fn decode_path(path: &str) -> Result<Vec<String>, DecodeError> {
    if path == "/" {
        return Ok(Vec::new());
    }

    debug_assert!(path.starts_with('/'));
    path[1..]
        .split('/')
        .map(|segment| percent_decode(segment, false))
        .collect()
}

fn parse_pattern(pattern: &str) -> Vec<Segment> {
    assert!(
        pattern.starts_with('/'),
        "route pattern must start with '/': {pattern}"
    );
    assert!(
        !pattern.chars().any(|c| c == '?' || c == '#'),
        "route pattern must contain only a path: {pattern}"
    );
    if pattern == "/" {
        return Vec::new();
    }

    let raw = &pattern[1..];
    let mut names = HashSet::new();
    let mut segments = Vec::new();

    for part in raw.split('/') {
        assert!(
            !part.is_empty(),
            "route pattern has an empty segment: {pattern}"
        );
        if let Some(name) = part.strip_prefix(':') {
            assert!(
                !name.is_empty()
                    && name
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '_'),
                "invalid route parameter name: {pattern}"
            );
            assert!(
                names.insert(name.to_string()),
                "duplicate parameter in {pattern}"
            );
            segments.push(Segment::Param(name.to_string()));
        } else {
            segments.push(Segment::Static(part.to_string()));
        }
    }

    segments
}

fn allow_header(methods: &[Method]) -> String {
    // A stable order makes Allow headers predictable and easy to test.
    const ORDER: [Method; 7] = [
        Method::Get,
        Method::Head,
        Method::Post,
        Method::Put,
        Method::Patch,
        Method::Delete,
        Method::Options,
    ];

    ORDER
        .iter()
        .filter(|method| methods.contains(method))
        .map(|method| method.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{read_request, Version};
    use std::io::Cursor;

    fn request(method: Method, target: &str) -> Request {
        let raw = format!(
            "{} {} HTTP/1.1\r\nHost: test\r\n\r\n",
            method.as_str(),
            target
        );
        read_request(&mut Cursor::new(raw.as_bytes()), 16 * 1024, 1024, || Ok(()))
            .unwrap()
            .unwrap()
    }

    fn ok(_: &Request, _: &Params) -> Response {
        Response::text(StatusCode::Ok, "ok")
    }

    fn id(_: &Request, params: &Params) -> Response {
        Response::text(StatusCode::Ok, params.get("id").unwrap_or("missing"))
    }

    fn wire(response: &Response) -> String {
        String::from_utf8(response.to_http_bytes(Version::Http11, false, false)).unwrap()
    }

    #[test]
    fn matches_static_and_parameter_routes() {
        let router = Router::new().get("/", ok).get("/users/:id", id);

        assert_eq!(
            router.dispatch(&request(Method::Get, "/")).status(),
            StatusCode::Ok
        );
        let response = router.dispatch(&request(Method::Get, "/users/42"));
        assert!(wire(&response).ends_with("\r\n\r\n42"));
    }

    #[test]
    fn decodes_path_segments_before_matching() {
        let router = Router::new().get("/users/:id", id);
        let response = router.dispatch(&request(Method::Get, "/users/%E4%B8%96%E7%95%8C"));
        assert!(wire(&response).ends_with("\r\n\r\n世界"));
    }

    #[test]
    fn reports_missing_routes_and_methods() {
        let router = Router::new().get("/users/:id", id);

        let missing = router.dispatch(&request(Method::Get, "/nope"));
        assert_eq!(missing.status(), StatusCode::NotFound);

        let wrong_method = router.dispatch(&request(Method::Post, "/users/1"));
        assert_eq!(wrong_method.status(), StatusCode::MethodNotAllowed);
        let wire = wire(&wrong_method);
        assert!(wire.contains("Allow: GET, HEAD\r\n"));
    }

    #[test]
    fn head_falls_back_to_get() {
        let router = Router::new().get("/health", ok);
        let response = router.dispatch(&request(Method::Head, "/health"));
        assert_eq!(response.status(), StatusCode::Ok);
    }

    #[test]
    fn rejects_invalid_percent_encoding() {
        let router = Router::new().get("/users/:id", id);
        let response = router.dispatch(&request(Method::Get, "/users/%ZZ"));
        assert_eq!(response.status(), StatusCode::BadRequest);
    }

    #[test]
    fn empty_parameter_does_not_match() {
        let router = Router::new().get("/users/:id", id);
        let response = router.dispatch(&request(Method::Get, "/users/"));
        assert_eq!(response.status(), StatusCode::NotFound);
    }
}
