use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use crate::error::WebError;
use crate::handler::ApiState;
use crate::request::parse_request;
use crate::response::{error_response, write_response};
use crate::router::Router;

#[derive(Debug, Clone)]
pub struct WebServer {
    addr: String,
    router: Router,
    state: ApiState,
    read_timeout: Duration,
    write_timeout: Duration,
}

impl WebServer {
    pub fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_string(),
            router: Router::new(),
            state: ApiState::default(),
            read_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(5),
        }
    }

    pub fn with_router(mut self, router: Router) -> Self {
        self.router = router;
        self
    }

    pub fn with_state(mut self, state: ApiState) -> Self {
        self.state = state;
        self
    }

    pub fn run(&self) -> Result<(), WebError> {
        let listener = TcpListener::bind(&self.addr)?;
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if let Err(err) = self.handle_connection(stream) {
                        eprintln!("failed to handle connection: {err}");
                    }
                }
                Err(err) => return Err(WebError::Io(err)),
            }
        }
        Ok(())
    }

    pub fn run_once(&self) -> Result<(), WebError> {
        let listener = TcpListener::bind(&self.addr)?;
        let (stream, _) = listener.accept()?;
        self.handle_connection(stream)
    }

    fn handle_connection(&self, mut stream: TcpStream) -> Result<(), WebError> {
        stream.set_read_timeout(Some(self.read_timeout))?;
        stream.set_write_timeout(Some(self.write_timeout))?;

        let request = match parse_request(&mut stream) {
            Ok(request) => request,
            Err(err) => {
                let response = error_response(400, &err.to_string());
                write_response(&mut stream, &response)?;
                return Ok(());
            }
        };

        let response = match self.router.match_route(&request.method, &request.path) {
            Some(route_match) => (route_match.handler)(&request, &self.state, &route_match.params),
            None => error_response(404, "route not found"),
        };

        write_response(&mut stream, &response)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    use crate::handler::{ApiState, handle_health};
    use crate::request::HttpMethod;
    use crate::router::Router;
    use crate::server::WebServer;

    fn free_address() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        listener.local_addr().expect("read local addr")
    }

    fn connect_with_retry(addr: SocketAddr) -> TcpStream {
        for _ in 0..30 {
            if let Ok(stream) = TcpStream::connect(addr) {
                return stream;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("failed to connect to server");
    }

    #[test]
    fn run_once_serves_health_endpoint() {
        let addr = free_address();
        let mut router = Router::new();
        router.add_route(HttpMethod::GET, "/api/v1/health", handle_health);

        let server = WebServer::new(&addr.to_string())
            .with_router(router)
            .with_state(ApiState::default());

        let handle = thread::spawn(move || server.run_once());

        let mut client = connect_with_retry(addr);
        client
            .write_all(b"GET /api/v1/health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("write request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("read response");

        let result = handle.join().expect("join thread");
        assert!(result.is_ok());
        assert!(response.starts_with("HTTP/1.1 200 OK"));
    }

    #[test]
    fn run_once_returns_not_found_for_unknown_route() {
        let addr = free_address();
        let server = WebServer::new(&addr.to_string());

        let handle = thread::spawn(move || server.run_once());

        let mut client = connect_with_retry(addr);
        client
            .write_all(b"GET /unknown HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("write request");

        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("read response");

        let result = handle.join().expect("join thread");
        assert!(result.is_ok());
        assert!(response.starts_with("HTTP/1.1 404 Not Found"));
    }
}
