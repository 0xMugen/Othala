use std::collections::HashMap;
use std::io::Write;
use std::net::TcpStream;

use serde::Serialize;

use crate::error::WebError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status_code: u16,
    pub status_text: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

pub fn json_response(status: u16, body: &impl Serialize) -> HttpResponse {
    let serialized = match serde_json::to_string(body) {
        Ok(value) => value,
        Err(_) => "{\"error\":\"failed to serialize response\"}".to_string(),
    };

    let mut headers = HashMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert("Connection".to_string(), "close".to_string());
    headers.insert("Content-Length".to_string(), serialized.len().to_string());

    HttpResponse {
        status_code: status,
        status_text: status_text(status).to_string(),
        headers,
        body: serialized,
    }
}

pub fn error_response(status: u16, message: &str) -> HttpResponse {
    json_response(status, &serde_json::json!({ "error": message }))
}

pub fn ok() -> HttpResponse {
    json_response(200, &serde_json::json!({ "status": "ok" }))
}

pub fn created() -> HttpResponse {
    json_response(201, &serde_json::json!({ "status": "created" }))
}

pub fn not_found() -> HttpResponse {
    error_response(404, "not found")
}

pub fn bad_request() -> HttpResponse {
    error_response(400, "bad request")
}

pub fn internal_error() -> HttpResponse {
    error_response(500, "internal server error")
}

pub fn write_response(stream: &mut TcpStream, response: &HttpResponse) -> Result<(), WebError> {
    let mut output = String::new();
    output.push_str(&format!(
        "HTTP/1.1 {} {}\r\n",
        response.status_code, response.status_text
    ));
    for (name, value) in &response.headers {
        output.push_str(&format!("{name}: {value}\r\n"));
    }
    output.push_str("\r\n");
    output.push_str(&response.body);

    stream.write_all(output.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use super::{HttpResponse, error_response, json_response, write_response};

    #[test]
    fn builds_json_response() {
        let response = json_response(200, &serde_json::json!({ "hello": "world" }));

        assert_eq!(response.status_code, 200);
        assert_eq!(response.status_text, "OK");
        assert_eq!(
            response.headers.get("Content-Type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(response.body, "{\"hello\":\"world\"}");
    }

    #[test]
    fn builds_error_response() {
        let response = error_response(404, "missing");

        assert_eq!(response.status_code, 404);
        assert_eq!(response.status_text, "Not Found");
        assert_eq!(response.body, "{\"error\":\"missing\"}");
    }

    #[test]
    fn writes_http_response_to_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let response = HttpResponse {
                status_code: 200,
                status_text: "OK".to_string(),
                headers: std::collections::HashMap::from([
                    ("Content-Type".to_string(), "application/json".to_string()),
                    ("Content-Length".to_string(), "2".to_string()),
                ]),
                body: "{}".to_string(),
            };
            write_response(&mut stream, &response).expect("write response");
        });

        let mut client = TcpStream::connect(addr).expect("connect");

        let mut received = String::new();
        client.read_to_string(&mut received).expect("read response");

        server.join().expect("join server");
        assert!(received.starts_with("HTTP/1.1 200 OK"));
        assert!(received.contains("Content-Type: application/json"));
        assert!(received.ends_with("{}"));
    }
}
