/// Response status codes implemented by the teaching server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusCode {
    Ok,
    Created,
    NoContent,
    BadRequest,
    NotFound,
    MethodNotAllowed,
    RequestTimeout,
    PayloadTooLarge,
    UriTooLong,
    UnsupportedMediaType,
    ExpectationFailed,
    RequestHeaderFieldsTooLarge,
    InternalServerError,
    NotImplemented,
    HttpVersionNotSupported,
    ServiceUnavailable,
}

impl StatusCode {
    pub const fn code(self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::Created => 201,
            Self::NoContent => 204,
            Self::BadRequest => 400,
            Self::NotFound => 404,
            Self::MethodNotAllowed => 405,
            Self::RequestTimeout => 408,
            Self::PayloadTooLarge => 413,
            Self::UriTooLong => 414,
            Self::UnsupportedMediaType => 415,
            Self::ExpectationFailed => 417,
            Self::RequestHeaderFieldsTooLarge => 431,
            Self::InternalServerError => 500,
            Self::NotImplemented => 501,
            Self::HttpVersionNotSupported => 505,
            Self::ServiceUnavailable => 503,
        }
    }

    pub const fn reason(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Created => "Created",
            Self::NoContent => "No Content",
            Self::BadRequest => "Bad Request",
            Self::NotFound => "Not Found",
            Self::MethodNotAllowed => "Method Not Allowed",
            Self::RequestTimeout => "Request Timeout",
            Self::PayloadTooLarge => "Payload Too Large",
            Self::UriTooLong => "URI Too Long",
            Self::UnsupportedMediaType => "Unsupported Media Type",
            Self::ExpectationFailed => "Expectation Failed",
            Self::RequestHeaderFieldsTooLarge => "Request Header Fields Too Large",
            Self::InternalServerError => "Internal Server Error",
            Self::NotImplemented => "Not Implemented",
            Self::HttpVersionNotSupported => "HTTP Version Not Supported",
            Self::ServiceUnavailable => "Service Unavailable",
        }
    }

    /// 1xx, 204 and 304 responses must not carry a body.
    pub const fn allows_body(self) -> bool {
        !matches!(self, Self::NoContent)
    }
}
