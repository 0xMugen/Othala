use crate::mcp::{
    INTERNAL_ERROR, JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpServer, PARSE_ERROR,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fmt::{self, Display};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

const DEFAULT_CORS_ORIGIN: &str = "*";
const DEFAULT_MAX_REQUEST_SIZE_BYTES: usize = 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const HTTP_HEADER_TERMINATOR: &[u8; 4] = b"\r\n\r\n";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportKind {
    Stdio,
    Http { bind_addr: String, port: u16 },
    Sse { bind_addr: String, port: u16 },
}

impl Display for TransportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdio => write!(f, "stdio"),
            Self::Http { bind_addr, port } => write!(f, "http://{bind_addr}:{port}"),
            Self::Sse { bind_addr, port } => write!(f, "sse://{bind_addr}:{port}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportConfig {
    pub kind: TransportKind,
    pub cors_origins: Vec<String>,
    pub max_request_size_bytes: usize,
    pub read_timeout_ms: u64,
    pub write_timeout_ms: u64,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            kind: TransportKind::Stdio,
            cors_origins: vec![DEFAULT_CORS_ORIGIN.to_string()],
            max_request_size_bytes: DEFAULT_MAX_REQUEST_SIZE_BYTES,
            read_timeout_ms: DEFAULT_TIMEOUT_MS,
            write_timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<u64>,
}

impl SseEvent {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        if let Some(event) = &self.event {
            out.push_str("event: ");
            out.push_str(event);
            out.push('\n');
        }

        for line in self.data.lines() {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }

        if self.data.is_empty() {
            out.push_str("data: \n");
        }

        if let Some(id) = &self.id {
            out.push_str("id: ");
            out.push_str(id);
            out.push('\n');
        }

        if let Some(retry) = self.retry {
            out.push_str("retry: ");
            out.push_str(&retry.to_string());
            out.push('\n');
        }

        out.push('\n');
        out.into_bytes()
    }
}

#[derive(Debug)]
pub struct SseStream {
    listeners: Vec<TcpStream>,
}

impl SseStream {
    pub fn new() -> Self {
        Self {
            listeners: Vec::new(),
        }
    }

    pub fn add_listener(&mut self, stream: TcpStream) {
        self.listeners.push(stream);
    }

    pub fn remove_listener(&mut self, index: usize) {
        if index < self.listeners.len() {
            self.listeners.remove(index);
        }
    }

    pub fn listener_count(&self) -> usize {
        self.listeners.len()
    }

    pub fn broadcast(&mut self, event: &SseEvent) -> Vec<usize> {
        let payload = event.to_bytes();
        Self::broadcast_to_writers(self.listeners.as_mut_slice(), &payload)
    }

    fn broadcast_to_writers<W: Write>(writers: &mut [W], payload: &[u8]) -> Vec<usize> {
        let mut failed = Vec::new();
        for (index, writer) in writers.iter_mut().enumerate() {
            if writer
                .write_all(payload)
                .and_then(|_| writer.flush())
                .is_err()
            {
                failed.push(index);
            }
        }

        failed
    }
}

impl Default for SseStream {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub enum TransportError {
    Io(io::Error),
    Json(serde_json::Error),
    InvalidHttpRequest(String),
    RequestTooLarge(usize),
    MethodNotAllowed(String),
    Timeout,
    ConnectionClosed,
}

impl Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Json(err) => write!(f, "JSON error: {err}"),
            Self::InvalidHttpRequest(message) => write!(f, "Invalid HTTP request: {message}"),
            Self::RequestTooLarge(size) => write!(f, "Request too large: {size} bytes"),
            Self::MethodNotAllowed(method) => write!(f, "Method not allowed: {method}"),
            Self::Timeout => write!(f, "Operation timed out"),
            Self::ConnectionClosed => write!(f, "Connection closed"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Json(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for TransportError {
    fn from(value: io::Error) -> Self {
        if value.kind() == io::ErrorKind::TimedOut {
            return Self::Timeout;
        }

        Self::Io(value)
    }
}

impl From<serde_json::Error> for TransportError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorsConfig {
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub allowed_headers: Vec<String>,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: vec![DEFAULT_CORS_ORIGIN.to_string()],
            allowed_methods: vec!["POST".to_string(), "GET".to_string(), "OPTIONS".to_string()],
            allowed_headers: vec!["Content-Type".to_string(), "Authorization".to_string()],
        }
    }
}

impl CorsConfig {
    pub fn cors_headers(&self) -> Vec<(String, String)> {
        vec![
            (
                "Access-Control-Allow-Origin".to_string(),
                if self.allowed_origins.iter().any(|origin| origin == "*") {
                    "*".to_string()
                } else {
                    self.allowed_origins.join(", ")
                },
            ),
            (
                "Access-Control-Allow-Methods".to_string(),
                self.allowed_methods.join(", "),
            ),
            (
                "Access-Control-Allow-Headers".to_string(),
                self.allowed_headers.join(", "),
            ),
        ]
    }

    pub fn is_origin_allowed(&self, origin: &str) -> bool {
        self.allowed_origins.iter().any(|allowed| {
            if allowed == "*" {
                return true;
            }

            allowed == origin
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Rpc,
    Sse,
    Options,
    Health,
    NotFound,
}

pub struct HttpTransport {
    config: TransportConfig,
    cors: CorsConfig,
    server: Mutex<McpServer>,
    sse_stream: Mutex<SseStream>,
}

impl HttpTransport {
    pub fn new(config: &TransportConfig) -> Result<Self, TransportError> {
        let mut server = McpServer::new();
        server.register_builtin_tools();

        Ok(Self {
            config: config.clone(),
            cors: CorsConfig {
                allowed_origins: config.cors_origins.clone(),
                ..CorsConfig::default()
            },
            server: Mutex::new(server),
            sse_stream: Mutex::new(SseStream::new()),
        })
    }

    pub fn bind(&self) -> Result<(), TransportError> {
        let bind_target = match &self.config.kind {
            TransportKind::Stdio => {
                return Err(TransportError::InvalidHttpRequest(
                    "stdio transport does not bind TCP listener".to_string(),
                ));
            }
            TransportKind::Http { bind_addr, port } | TransportKind::Sse { bind_addr, port } => {
                format!("{bind_addr}:{port}")
            }
        };

        let listener = TcpListener::bind(&bind_target)?;
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if let Err(err) = self.handle_connection(stream) {
                        eprintln!("mcp transport connection error: {err}");
                    }
                }
                Err(err) => return Err(TransportError::Io(err)),
            }
        }

        Ok(())
    }

    pub fn handle_connection(&self, mut stream: TcpStream) -> Result<(), TransportError> {
        stream.set_read_timeout(Some(Duration::from_millis(self.config.read_timeout_ms)))?;
        stream.set_write_timeout(Some(Duration::from_millis(self.config.write_timeout_ms)))?;

        let raw_request = self.read_http_request(&mut stream)?;
        let request = Self::parse_http_request(&raw_request)?;
        self.dispatch_request(request, &mut stream)
    }

    pub fn parse_http_request(raw: &[u8]) -> Result<HttpRequest, TransportError> {
        let header_end = raw
            .windows(HTTP_HEADER_TERMINATOR.len())
            .position(|window| window == HTTP_HEADER_TERMINATOR)
            .map(|index| index + HTTP_HEADER_TERMINATOR.len())
            .ok_or_else(|| {
                TransportError::InvalidHttpRequest("missing header terminator".to_string())
            })?;

        let header_bytes = &raw[..header_end];
        let header_text = std::str::from_utf8(header_bytes)
            .map_err(|_| TransportError::InvalidHttpRequest("headers must be UTF-8".to_string()))?;

        let mut lines = header_text.split("\r\n");
        let request_line = lines.next().ok_or_else(|| {
            TransportError::InvalidHttpRequest("missing HTTP request line".to_string())
        })?;

        let mut request_parts = request_line.split_whitespace();
        let method = request_parts
            .next()
            .ok_or_else(|| TransportError::InvalidHttpRequest("missing HTTP method".to_string()))?;
        let path = request_parts
            .next()
            .ok_or_else(|| TransportError::InvalidHttpRequest("missing request path".to_string()))?;
        let version = request_parts
            .next()
            .ok_or_else(|| TransportError::InvalidHttpRequest("missing HTTP version".to_string()))?;

        if !version.starts_with("HTTP/1.") {
            return Err(TransportError::InvalidHttpRequest(format!(
                "unsupported HTTP version: {version}"
            )));
        }

        let mut headers = HashMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }

            let (name, value) = line.split_once(':').ok_or_else(|| {
                TransportError::InvalidHttpRequest(format!("malformed header: {line}"))
            })?;
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }

        let content_length = headers
            .get("content-length")
            .map(|value| {
                value.parse::<usize>().map_err(|_| {
                    TransportError::InvalidHttpRequest("invalid content-length".to_string())
                })
            })
            .transpose()?
            .unwrap_or(0);

        let body_bytes = &raw[header_end..];
        if body_bytes.len() < content_length {
            return Err(TransportError::InvalidHttpRequest(
                "request body is shorter than content-length".to_string(),
            ));
        }

        let body = if content_length > 0 {
            String::from_utf8(body_bytes[..content_length].to_vec()).map_err(|_| {
                TransportError::InvalidHttpRequest("body must be valid UTF-8".to_string())
            })?
        } else if !body_bytes.is_empty() {
            String::from_utf8(body_bytes.to_vec()).map_err(|_| {
                TransportError::InvalidHttpRequest("body must be valid UTF-8".to_string())
            })?
        } else {
            String::new()
        };

        Ok(HttpRequest {
            method: method.to_string(),
            path: path.to_string(),
            headers,
            body,
        })
    }

    pub fn build_http_response(status: u16, headers: &[(&str, &str)], body: &str) -> Vec<u8> {
        let mut response = String::new();
        response.push_str("HTTP/1.1 ");
        response.push_str(&status.to_string());
        response.push(' ');
        response.push_str(status_text(status));
        response.push_str("\r\n");

        for (name, value) in headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }

        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        {
            response.push_str("Content-Length: ");
            response.push_str(&body.len().to_string());
            response.push_str("\r\n");
        }

        response.push_str("\r\n");
        response.push_str(body);
        response.into_bytes()
    }

    pub fn handle_post_rpc(&self, body: &str) -> String {
        let request = match serde_json::from_str::<JsonRpcRequest>(body) {
            Ok(request) => request,
            Err(err) => {
                let response = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: None,
                    result: None,
                    error: Some(JsonRpcError {
                        code: PARSE_ERROR,
                        message: "Parse error".to_string(),
                        data: Some(json!({ "reason": err.to_string() })),
                    }),
                };
                return safe_serialize_json_rpc_response(response);
            }
        };

        let is_notification = request.id.is_none();
        let response = match self.server.lock() {
            Ok(mut server) => server.handle_request(&request),
            Err(err) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id.clone(),
                result: None,
                error: Some(JsonRpcError {
                    code: INTERNAL_ERROR,
                    message: "MCP server mutex poisoned".to_string(),
                    data: Some(json!({ "reason": err.to_string() })),
                }),
            },
        };

        if is_notification {
            String::new()
        } else {
            safe_serialize_json_rpc_response(response)
        }
    }

    pub fn handle_get_sse(&self, stream: &mut TcpStream) -> Result<(), TransportError> {
        let mut headers = vec![
            ("Content-Type", "text/event-stream"),
            ("Cache-Control", "no-cache"),
            ("Connection", "keep-alive"),
        ];
        let cors_headers = self.cors.cors_headers();
        let mut owned_pairs = Vec::new();
        for (name, value) in cors_headers {
            owned_pairs.push((name, value));
        }
        for (name, value) in &owned_pairs {
            headers.push((name.as_str(), value.as_str()));
        }

        let response = Self::build_http_response(200, &headers, "");
        stream.write_all(&response)?;
        stream.flush()?;

        let listener = stream.try_clone()?;
        if let Ok(mut sse_stream) = self.sse_stream.lock() {
            sse_stream.add_listener(listener);
        }

        loop {
            if stream.write_all(b": keep-alive\n\n").is_err() {
                return Err(TransportError::ConnectionClosed);
            }
            stream.flush()?;
            thread::sleep(Duration::from_secs(15));
        }
    }

    pub fn broadcast_event(&self, event: &SseEvent) -> Vec<usize> {
        let Ok(mut stream) = self.sse_stream.lock() else {
            return Vec::new();
        };

        let mut failed = stream.broadcast(event);
        failed.sort_unstable();
        for &index in failed.iter().rev() {
            stream.remove_listener(index);
        }

        failed
    }

    fn dispatch_request(
        &self,
        request: HttpRequest,
        stream: &mut TcpStream,
    ) -> Result<(), TransportError> {
        let route = Self::route_request(&request);
        match route {
            Ok(Route::Rpc) => {
                if request.body.trim().is_empty() {
                    let response = self.build_json_response(
                        400,
                        &json!({"error": "empty request body"}).to_string(),
                    );
                    stream.write_all(&response)?;
                    stream.flush()?;
                    return Ok(());
                }

                let rpc_response = self.handle_post_rpc(&request.body);
                if rpc_response.is_empty() {
                    let response = self.build_plain_response(204, "");
                    stream.write_all(&response)?;
                    stream.flush()?;
                    return Ok(());
                }

                let response = self.build_json_response(200, &rpc_response);
                stream.write_all(&response)?;
                stream.flush()?;
                Ok(())
            }
            Ok(Route::Sse) => self.handle_get_sse(stream),
            Ok(Route::Options) => {
                let response = self.build_plain_response(204, "");
                stream.write_all(&response)?;
                stream.flush()?;
                Ok(())
            }
            Ok(Route::Health) => {
                let response = self.build_json_response(200, &json!({"status": "ok"}).to_string());
                stream.write_all(&response)?;
                stream.flush()?;
                Ok(())
            }
            Ok(Route::NotFound) => {
                let response = self.build_json_response(404, &json!({"error": "not found"}).to_string());
                stream.write_all(&response)?;
                stream.flush()?;
                Ok(())
            }
            Err(err) => {
                let response = self.build_json_response(
                    405,
                    &json!({"error": err.to_string()}).to_string(),
                );
                stream.write_all(&response)?;
                stream.flush()?;
                Ok(())
            }
        }
    }

    fn route_request(request: &HttpRequest) -> Result<Route, TransportError> {
        let method = request.method.as_str();
        let path = request.path.as_str();

        match (method, path) {
            ("POST", "/rpc") => Ok(Route::Rpc),
            ("GET", "/sse") => Ok(Route::Sse),
            ("OPTIONS", _) => Ok(Route::Options),
            ("GET", "/health") => Ok(Route::Health),
            ("POST", "/sse") | ("GET", "/rpc") | ("POST", "/health") | ("GET", "/") => {
                Err(TransportError::MethodNotAllowed(method.to_string()))
            }
            (_, "/rpc") | (_, "/sse") | (_, "/health") => {
                Err(TransportError::MethodNotAllowed(method.to_string()))
            }
            _ => Ok(Route::NotFound),
        }
    }

    fn read_http_request(&self, stream: &mut TcpStream) -> Result<Vec<u8>, TransportError> {
        let mut bytes = Vec::new();
        let mut header_end = None;

        loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                if bytes.is_empty() {
                    return Err(TransportError::ConnectionClosed);
                }
                break;
            }

            bytes.extend_from_slice(&chunk[..read]);
            Self::enforce_request_size(bytes.len(), self.config.max_request_size_bytes)?;

            if let Some(pos) = bytes
                .windows(HTTP_HEADER_TERMINATOR.len())
                .position(|window| window == HTTP_HEADER_TERMINATOR)
            {
                header_end = Some(pos + HTTP_HEADER_TERMINATOR.len());
                break;
            }
        }

        let header_end = header_end.ok_or_else(|| {
            TransportError::InvalidHttpRequest("request missing header terminator".to_string())
        })?;

        let content_length = parse_content_length(&bytes[..header_end])?;
        let target_len = header_end + content_length;
        while bytes.len() < target_len {
            let mut chunk = vec![0_u8; target_len - bytes.len()];
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                return Err(TransportError::ConnectionClosed);
            }

            bytes.extend_from_slice(&chunk[..read]);
            Self::enforce_request_size(bytes.len(), self.config.max_request_size_bytes)?;
        }

        Ok(bytes)
    }

    fn enforce_request_size(size: usize, max_request_size_bytes: usize) -> Result<(), TransportError> {
        if size > max_request_size_bytes {
            return Err(TransportError::RequestTooLarge(size));
        }
        Ok(())
    }

    fn build_json_response(&self, status: u16, body: &str) -> Vec<u8> {
        let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        headers.extend(self.cors.cors_headers());
        self.build_response_with_owned_headers(status, &headers, body)
    }

    fn build_plain_response(&self, status: u16, body: &str) -> Vec<u8> {
        let mut headers = vec![("Content-Type".to_string(), "text/plain; charset=utf-8".to_string())];
        headers.extend(self.cors.cors_headers());
        self.build_response_with_owned_headers(status, &headers, body)
    }

    fn build_response_with_owned_headers(
        &self,
        status: u16,
        headers: &[(String, String)],
        body: &str,
    ) -> Vec<u8> {
        let borrowed_headers = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        Self::build_http_response(status, &borrowed_headers, body)
    }
}

fn parse_content_length(header_bytes: &[u8]) -> Result<usize, TransportError> {
    let text = std::str::from_utf8(header_bytes)
        .map_err(|_| TransportError::InvalidHttpRequest("headers must be valid UTF-8".to_string()))?;
    for line in text.split("\r\n") {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                return value.trim().parse::<usize>().map_err(|_| {
                    TransportError::InvalidHttpRequest("invalid content-length".to_string())
                });
            }
        }
    }

    Ok(0)
}

fn safe_serialize_json_rpc_response(response: JsonRpcResponse) -> String {
    match serde_json::to_string(&response) {
        Ok(serialized) => serialized,
        Err(err) => {
            let fallback = json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {
                    "code": INTERNAL_ERROR,
                    "message": "failed to serialize response",
                    "data": {
                        "reason": err.to_string()
                    }
                }
            });
            fallback.to_string()
        }
    }
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::io::BufWriter;

    struct FailWriter;

    impl Write for FailWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn make_http_transport(kind: TransportKind) -> HttpTransport {
        HttpTransport::new(&TransportConfig {
            kind,
            ..TransportConfig::default()
        })
        .expect("transport creates")
    }

    #[test]
    fn transport_config_default_is_stdio_with_expected_limits() {
        let config = TransportConfig::default();
        assert_eq!(config.kind, TransportKind::Stdio);
        assert_eq!(config.cors_origins, vec!["*".to_string()]);
        assert_eq!(config.max_request_size_bytes, 1024 * 1024);
        assert_eq!(config.read_timeout_ms, 30_000);
        assert_eq!(config.write_timeout_ms, 30_000);
    }

    #[test]
    fn transport_config_serialization_round_trip() {
        let config = TransportConfig {
            kind: TransportKind::Http {
                bind_addr: "127.0.0.1".to_string(),
                port: 8080,
            },
            cors_origins: vec!["https://example.com".to_string()],
            max_request_size_bytes: 2048,
            read_timeout_ms: 10_000,
            write_timeout_ms: 12_000,
        };

        let serialized = serde_json::to_string(&config).expect("serialize config");
        let deserialized: TransportConfig = serde_json::from_str(&serialized).expect("deserialize config");
        assert_eq!(deserialized, config);
    }

    #[test]
    fn transport_kind_serialization_round_trip() {
        let kind = TransportKind::Sse {
            bind_addr: "0.0.0.0".to_string(),
            port: 9123,
        };
        let serialized = serde_json::to_string(&kind).expect("serialize kind");
        let deserialized: TransportKind = serde_json::from_str(&serialized).expect("deserialize kind");
        assert_eq!(deserialized, kind);
    }

    #[test]
    fn parse_http_request_valid_post() {
        let raw = b"POST /rpc HTTP/1.1\r\nHost: localhost\r\nContent-Length: 29\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}";
        let parsed = HttpTransport::parse_http_request(raw).expect("parse post");
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.path, "/rpc");
        assert_eq!(parsed.headers.get("host").map(String::as_str), Some("localhost"));
        assert!(parsed.body.starts_with('{'));
    }

    #[test]
    fn parse_http_request_valid_get() {
        let raw = b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let parsed = HttpTransport::parse_http_request(raw).expect("parse get");
        assert_eq!(parsed.method, "GET");
        assert_eq!(parsed.path, "/health");
        assert_eq!(parsed.body, "");
    }

    #[test]
    fn parse_http_request_valid_options() {
        let raw = b"OPTIONS /rpc HTTP/1.1\r\nOrigin: https://example.com\r\n\r\n";
        let parsed = HttpTransport::parse_http_request(raw).expect("parse options");
        assert_eq!(parsed.method, "OPTIONS");
        assert_eq!(parsed.path, "/rpc");
        assert_eq!(
            parsed.headers.get("origin").map(String::as_str),
            Some("https://example.com")
        );
    }

    #[test]
    fn parse_http_request_rejects_missing_terminator() {
        let raw = b"GET /health HTTP/1.1\r\nHost: localhost\r\n";
        let error = HttpTransport::parse_http_request(raw).expect_err("must fail");
        assert!(matches!(error, TransportError::InvalidHttpRequest(_)));
    }

    #[test]
    fn parse_http_request_rejects_malformed_header() {
        let raw = b"GET /health HTTP/1.1\r\nHost localhost\r\n\r\n";
        let error = HttpTransport::parse_http_request(raw).expect_err("must fail");
        assert!(matches!(error, TransportError::InvalidHttpRequest(_)));
    }

    #[test]
    fn parse_http_request_rejects_short_body_for_content_length() {
        let raw = b"POST /rpc HTTP/1.1\r\nContent-Length: 5\r\n\r\n{}";
        let error = HttpTransport::parse_http_request(raw).expect_err("must fail");
        assert!(matches!(error, TransportError::InvalidHttpRequest(_)));
    }

    #[test]
    fn build_http_response_includes_status_headers_and_body() {
        let response = HttpTransport::build_http_response(
            200,
            &[
                ("Content-Type", "application/json"),
                ("Access-Control-Allow-Origin", "*"),
            ],
            "{\"ok\":true}",
        );
        let text = String::from_utf8(response).expect("utf-8 response");
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Access-Control-Allow-Origin: *\r\n"));
        assert!(text.contains("Content-Length: 11\r\n"));
        assert!(text.ends_with("{\"ok\":true}"));
    }

    #[test]
    fn sse_event_to_bytes_with_all_fields() {
        let event = SseEvent {
            event: Some("toolsChanged".to_string()),
            data: "line1\nline2".to_string(),
            id: Some("event-1".to_string()),
            retry: Some(3000),
        };

        let bytes = event.to_bytes();
        let text = String::from_utf8(bytes).expect("utf-8 event");
        assert!(text.contains("event: toolsChanged\n"));
        assert!(text.contains("data: line1\n"));
        assert!(text.contains("data: line2\n"));
        assert!(text.contains("id: event-1\n"));
        assert!(text.contains("retry: 3000\n"));
        assert!(text.ends_with("\n\n"));
    }

    #[test]
    fn sse_event_to_bytes_without_optional_fields() {
        let event = SseEvent {
            event: None,
            data: "ping".to_string(),
            id: None,
            retry: None,
        };

        let bytes = event.to_bytes();
        let text = String::from_utf8(bytes).expect("utf-8 event");
        assert_eq!(text, "data: ping\n\n");
    }

    #[test]
    fn sse_event_to_bytes_empty_data_line() {
        let event = SseEvent {
            event: None,
            data: String::new(),
            id: None,
            retry: None,
        };

        let bytes = event.to_bytes();
        let text = String::from_utf8(bytes).expect("utf-8 event");
        assert_eq!(text, "data: \n\n");
    }

    #[test]
    fn sse_stream_listener_tracking() {
        let stream = SseStream::new();
        assert_eq!(stream.listener_count(), 0);
    }

    #[test]
    fn sse_stream_broadcast_to_multiple_listeners_with_mock_writers() {
        let event = SseEvent {
            event: Some("ping".to_string()),
            data: "hello".to_string(),
            id: None,
            retry: None,
        };
        let payload = event.to_bytes();

        let mut writer_a = BufWriter::new(Vec::new());
        let mut writer_b = BufWriter::new(Vec::new());
        let failed = SseStream::broadcast_to_writers(
            &mut [&mut writer_a as &mut dyn Write, &mut writer_b as &mut dyn Write],
            &payload,
        );
        assert!(failed.is_empty());

        let bytes_a = writer_a.into_inner().expect("writer a into inner");
        let bytes_b = writer_b.into_inner().expect("writer b into inner");
        assert_eq!(bytes_a, payload);
        assert_eq!(bytes_b, payload);
    }

    #[test]
    fn sse_stream_broadcast_reports_failed_writer_indices() {
        let event = SseEvent {
            event: Some("ping".to_string()),
            data: "hello".to_string(),
            id: None,
            retry: None,
        };
        let payload = event.to_bytes();

        let mut writer = BufWriter::new(Vec::new());
        let mut fail = FailWriter;
        let failed = SseStream::broadcast_to_writers(
            &mut [&mut writer as &mut dyn Write, &mut fail as &mut dyn Write],
            &payload,
        );

        assert_eq!(failed, vec![1]);
    }

    #[test]
    fn cors_config_origin_exact_match() {
        let cors = CorsConfig {
            allowed_origins: vec!["https://app.example.com".to_string()],
            ..CorsConfig::default()
        };

        assert!(cors.is_origin_allowed("https://app.example.com"));
        assert!(!cors.is_origin_allowed("https://evil.example.com"));
    }

    #[test]
    fn cors_config_origin_wildcard_match() {
        let cors = CorsConfig::default();
        assert!(cors.is_origin_allowed("https://any-origin.example"));
    }

    #[test]
    fn cors_headers_include_required_fields() {
        let cors = CorsConfig::default();
        let headers = cors.cors_headers();
        let keys = headers.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();
        assert!(keys.contains(&"Access-Control-Allow-Origin".to_string()));
        assert!(keys.contains(&"Access-Control-Allow-Methods".to_string()));
        assert!(keys.contains(&"Access-Control-Allow-Headers".to_string()));
    }

    #[test]
    fn request_size_limit_enforcement() {
        let result = HttpTransport::enforce_request_size(1025, 1024);
        assert!(matches!(result, Err(TransportError::RequestTooLarge(1025))));
    }

    #[test]
    fn route_request_matches_rpc_endpoint() {
        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/rpc".to_string(),
            headers: HashMap::new(),
            body: "{}".to_string(),
        };

        let route = HttpTransport::route_request(&request).expect("route");
        assert_eq!(route, Route::Rpc);
    }

    #[test]
    fn route_request_matches_sse_endpoint() {
        let request = HttpRequest {
            method: "GET".to_string(),
            path: "/sse".to_string(),
            headers: HashMap::new(),
            body: String::new(),
        };

        let route = HttpTransport::route_request(&request).expect("route");
        assert_eq!(route, Route::Sse);
    }

    #[test]
    fn route_request_matches_health_endpoint() {
        let request = HttpRequest {
            method: "GET".to_string(),
            path: "/health".to_string(),
            headers: HashMap::new(),
            body: String::new(),
        };

        let route = HttpTransport::route_request(&request).expect("route");
        assert_eq!(route, Route::Health);
    }

    #[test]
    fn invalid_http_method_returns_method_not_allowed() {
        let request = HttpRequest {
            method: "PUT".to_string(),
            path: "/rpc".to_string(),
            headers: HashMap::new(),
            body: "{}".to_string(),
        };

        let error = HttpTransport::route_request(&request).expect_err("method not allowed");
        assert!(matches!(error, TransportError::MethodNotAllowed(method) if method == "PUT"));
    }

    #[test]
    fn route_request_returns_not_found_for_unknown_path() {
        let request = HttpRequest {
            method: "GET".to_string(),
            path: "/unknown".to_string(),
            headers: HashMap::new(),
            body: String::new(),
        };

        let route = HttpTransport::route_request(&request).expect("route");
        assert_eq!(route, Route::NotFound);
    }

    #[test]
    fn json_rpc_over_http_round_trip_initialize() {
        let transport = make_http_transport(TransportKind::Http {
            bind_addr: "127.0.0.1".to_string(),
            port: 8080,
        });

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let response_raw = transport.handle_post_rpc(body);
        let response: JsonRpcResponse = serde_json::from_str(&response_raw).expect("json-rpc response");
        assert!(response.error.is_none());
        assert_eq!(response.id, Some(json!(1)));
        assert_eq!(response.result.expect("result")["serverInfo"]["name"], json!("othala"));
    }

    #[test]
    fn json_rpc_over_http_round_trip_tools_list_after_initialize() {
        let transport = make_http_transport(TransportKind::Http {
            bind_addr: "127.0.0.1".to_string(),
            port: 8081,
        });

        let _ = transport.handle_post_rpc(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        );
        let response_raw = transport.handle_post_rpc(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        );
        let response: JsonRpcResponse = serde_json::from_str(&response_raw).expect("json-rpc response");
        assert!(response.error.is_none());
        let tools = response.result.expect("tools result")["tools"]
            .as_array()
            .expect("array")
            .clone();
        assert!(!tools.is_empty());
    }

    #[test]
    fn handle_post_rpc_invalid_json_returns_parse_error() {
        let transport = make_http_transport(TransportKind::Http {
            bind_addr: "127.0.0.1".to_string(),
            port: 8082,
        });

        let response_raw = transport.handle_post_rpc("{");
        let response: JsonRpcResponse = serde_json::from_str(&response_raw).expect("json-rpc response");
        let error = response.error.expect("parse error");
        assert_eq!(error.code, PARSE_ERROR);
    }

    #[test]
    fn handle_post_rpc_notification_returns_empty_body() {
        let transport = make_http_transport(TransportKind::Http {
            bind_addr: "127.0.0.1".to_string(),
            port: 8083,
        });

        let response_raw = transport.handle_post_rpc(
            r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        );
        assert_eq!(response_raw, "");
    }

    #[test]
    fn empty_body_handling_for_rpc_route() {
        let transport = make_http_transport(TransportKind::Http {
            bind_addr: "127.0.0.1".to_string(),
            port: 8084,
        });

        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/rpc".to_string(),
            headers: HashMap::new(),
            body: String::new(),
        };

        let route = HttpTransport::route_request(&request).expect("route");
        assert_eq!(route, Route::Rpc);

        let response = transport.build_json_response(400, &json!({ "error": "empty request body" }).to_string());
        let text = String::from_utf8(response).expect("utf-8");
        assert!(text.starts_with("HTTP/1.1 400 Bad Request\r\n"));
    }

    #[test]
    fn health_endpoint_response_payload() {
        let transport = make_http_transport(TransportKind::Http {
            bind_addr: "127.0.0.1".to_string(),
            port: 8085,
        });
        let response = transport.build_json_response(200, &json!({ "status": "ok" }).to_string());
        let text = String::from_utf8(response).expect("utf-8");
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.ends_with("{\"status\":\"ok\"}"));
    }

    #[test]
    fn parse_content_length_reads_value_case_insensitive() {
        let header = b"POST /rpc HTTP/1.1\r\nContent-Length: 13\r\n\r\n";
        let content_length = parse_content_length(header).expect("content-length");
        assert_eq!(content_length, 13);
    }

    #[test]
    fn parse_content_length_defaults_to_zero() {
        let header = b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let content_length = parse_content_length(header).expect("content-length");
        assert_eq!(content_length, 0);
    }

    #[test]
    fn transport_error_display_variants() {
        let io_err = TransportError::Io(io::Error::other("boom"));
        let json_err = TransportError::Json(
            serde_json::from_str::<serde_json::Value>("{").expect_err("json error"),
        );
        let invalid = TransportError::InvalidHttpRequest("bad".to_string());
        let too_large = TransportError::RequestTooLarge(10);
        let method = TransportError::MethodNotAllowed("TRACE".to_string());

        assert!(io_err.to_string().contains("I/O error"));
        assert!(json_err.to_string().contains("JSON error"));
        assert_eq!(invalid.to_string(), "Invalid HTTP request: bad");
        assert_eq!(too_large.to_string(), "Request too large: 10 bytes");
        assert_eq!(method.to_string(), "Method not allowed: TRACE");
        assert_eq!(TransportError::Timeout.to_string(), "Operation timed out");
        assert_eq!(TransportError::ConnectionClosed.to_string(), "Connection closed");
    }
}
