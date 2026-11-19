use std::fmt;

/// Errors produced while decoding percent-encoded URL components.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    InvalidEscape,
    InvalidUtf8,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEscape => f.write_str("invalid percent-encoding"),
            Self::InvalidUtf8 => f.write_str("decoded value is not valid UTF-8"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decode `%XX` escapes in a URL component.
///
/// In query strings, `+` conventionally means a space; path segments treat it
/// as a literal plus sign.
pub fn percent_decode(input: &str, plus_as_space: bool) -> Result<String, DecodeError> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index + 2 >= bytes.len() {
                    return Err(DecodeError::InvalidEscape);
                }
                let high = hex_value(bytes[index + 1]).ok_or(DecodeError::InvalidEscape)?;
                let low = hex_value(bytes[index + 2]).ok_or(DecodeError::InvalidEscape)?;
                decoded.push((high << 4) | low);
                index += 3;
            }
            b'+' if plus_as_space => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }

    String::from_utf8(decoded).map_err(|_| DecodeError::InvalidUtf8)
}

/// Decode a query string into key/value pairs. Duplicate keys are preserved.
pub fn query_pairs(query: &str) -> Result<Vec<(String, String)>, DecodeError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }

    query
        .split('&')
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            Ok((percent_decode(key, true)?, percent_decode(value, true)?))
        })
        .collect()
}

/// Return the first value for `name`, preserving standard "first wins"
/// behavior for duplicate query parameters.
pub fn query_param(query: Option<&str>, name: &str) -> Result<Option<String>, DecodeError> {
    let Some(query) = query else {
        return Ok(None);
    };

    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if percent_decode(key, true)? == name {
            return percent_decode(value, true).map(Some);
        }
    }

    Ok(None)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_path_and_query_rules() {
        assert_eq!(
            percent_decode("hello%20world", false).unwrap(),
            "hello world"
        );
        assert_eq!(percent_decode("a+b", false).unwrap(), "a+b");
        assert_eq!(percent_decode("a+b", true).unwrap(), "a b");
        assert_eq!(percent_decode("%E4%B8%96%E7%95%8C", false).unwrap(), "世界");
    }

    #[test]
    fn rejects_bad_escapes_and_utf8() {
        assert_eq!(
            percent_decode("%zz", false),
            Err(DecodeError::InvalidEscape)
        );
        assert_eq!(percent_decode("%2", false), Err(DecodeError::InvalidEscape));
        assert_eq!(percent_decode("%FF", false), Err(DecodeError::InvalidUtf8));
    }

    #[test]
    fn parses_query_pairs() {
        let pairs = query_pairs("name=%E4%BD%A0&x=a+b&flag").unwrap();
        assert_eq!(
            pairs,
            vec![
                ("name".to_string(), "你".to_string()),
                ("x".to_string(), "a b".to_string()),
                ("flag".to_string(), String::new()),
            ]
        );
        assert_eq!(
            query_param(Some("name=first&name=second"), "name").unwrap(),
            Some("first".to_string())
        );
        assert_eq!(query_param(None, "name").unwrap(), None);
    }
}
