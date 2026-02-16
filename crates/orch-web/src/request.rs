use std::collections::HashMap;
use std::io::Read;
use std::net::TcpStream;

use crate::error::WebError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpMethod {
    GET,
    POST,
    PUT,
    DELETE,
    PATCH,
}

impl HttpMethod {
    fn from_str(value: &str) -> Result<Self, WebError> {
        match value {
            "GET" => Ok(Self::GET),
            "POST" => Ok(Self::POST),
            "PUT" => Ok(Self::PUT),
            "DELETE" => Ok(Self::DELETE),
            "PATCH" => Ok(Self::PATCH),
            other => Err(WebError::Parse(format!("unsupported method: {other}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub path: String,
    pub query_params: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
}

pub fn parse_request(stream: &mut TcpStream) -> Result<HttpRequest, WebError> {
    const MAX_HEADER_BYTES: usize = 64 * 1024;
    const CHUNK_SIZE: usize = 1024;

    let mut bytes = Vec::new();
    let mut header_end = None;

    loop {
        let mut chunk = [0_u8; CHUNK_SIZE];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);

        if bytes.len() > MAX_HEADER_BYTES {
            return Err(WebError::Parse("request headers too large".to_string()));
        }

        if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = Some(pos + 4);
            break;
        }
    }

    let header_end = header_end.ok_or_else(|| WebError::Parse("incomplete request".to_string()))?;
    let header_raw = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| WebError::Parse("headers are not valid UTF-8".to_string()))?;

    let mut lines = header_raw.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| WebError::Parse("missing request line".to_string()))?;

    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| WebError::Parse("missing HTTP method".to_string()))?;
    let target = request_parts
        .next()
        .ok_or_else(|| WebError::Parse("missing request path".to_string()))?;
    let version = request_parts
        .next()
        .ok_or_else(|| WebError::Parse("missing HTTP version".to_string()))?;

    if !version.starts_with("HTTP/1.") {
        return Err(WebError::Parse(format!("unsupported HTTP version: {version}")));
    }

    let method = HttpMethod::from_str(method)?;
    let (path, query_params) = split_target(target);

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| WebError::Parse(format!("malformed header: {line}")))?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    let content_length = headers
        .get("content-length")
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| WebError::Parse("invalid content-length".to_string()))
        })
        .transpose()?
        .unwrap_or(0);

    let mut body_bytes = bytes[header_end..].to_vec();
    while body_bytes.len() < content_length {
        let mut chunk = vec![0_u8; content_length - body_bytes.len()];
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(WebError::Parse("request body truncated".to_string()));
        }
        body_bytes.extend_from_slice(&chunk[..read]);
    }

    let body = if content_length > 0 {
        let body = String::from_utf8(body_bytes[..content_length].to_vec())
            .map_err(|_| WebError::Parse("body is not valid UTF-8".to_string()))?;
        Some(body)
    } else if !body_bytes.is_empty() {
        let body = String::from_utf8(body_bytes)
            .map_err(|_| WebError::Parse("body is not valid UTF-8".to_string()))?;
        Some(body)
    } else {
        None
    };

    Ok(HttpRequest {
        method,
        path,
        query_params,
        headers,
        body,
    })
}

fn split_target(target: &str) -> (String, HashMap<String, String>) {
    let mut query_params = HashMap::new();
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (target, None),
    };

    if let Some(query) = query {
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => (pair, ""),
            };
            query_params.insert(key.to_string(), value.to_string());
        }
    }

    (path.to_string(), query_params)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;

    use super::{HttpMethod, parse_request};

    fn parse_from_wire(raw: &str) -> Result<super::HttpRequest, crate::error::WebError> {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let (tx, rx) = mpsc::channel();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let parsed = parse_request(&mut stream);
            tx.send(parsed).expect("send parse result");
        });

        let mut client = TcpStream::connect(addr).expect("connect to listener");
        client
            .write_all(raw.as_bytes())
            .expect("write raw request");
        let _ = client.shutdown(std::net::Shutdown::Write);

        server.join().expect("join server thread");
        rx.recv().expect("receive parse result")
    }

    #[test]
    fn parses_get_request_with_query() {
        let request = parse_from_wire("GET /api/v1/tasks?limit=10&state=ready HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("request should parse");

        assert_eq!(request.method, HttpMethod::GET);
        assert_eq!(request.path, "/api/v1/tasks");
        assert_eq!(request.query_params.get("limit").map(String::as_str), Some("10"));
        assert_eq!(request.query_params.get("state").map(String::as_str), Some("ready"));
        assert_eq!(request.headers.get("host").map(String::as_str), Some("localhost"));
        assert_eq!(request.body, None);
    }

    #[test]
    fn parses_post_request_with_body() {
        let raw = "POST /api/v1/tasks HTTP/1.1\r\nHost: localhost\r\nContent-Length: 16\r\n\r\n{\"title\":\"test\"}";
        let request = parse_from_wire(raw).expect("request should parse");

        assert_eq!(request.method, HttpMethod::POST);
        assert_eq!(request.path, "/api/v1/tasks");
        assert_eq!(request.body.as_deref(), Some("{\"title\":\"test\"}"));
    }

    #[test]
    fn rejects_malformed_request_line() {
        let result = parse_from_wire("GET_ONLY\r\n\r\n");

        assert!(result.is_err());
    }

    #[test]
    fn rejects_unsupported_method() {
        let result = parse_from_wire("TRACE /api/v1/health HTTP/1.1\r\nHost: localhost\r\n\r\n");

        assert!(result.is_err());
    }
}
