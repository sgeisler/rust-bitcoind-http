//! This module implements a minimal and non standard conforming HTTP client that works with
//! the bitcoind RPC server. This client can be used if minimal dependencies are a goal.

extern crate http;
extern crate jsonrpc;

use jsonrpc::client::HttpRoundTripper;

use http::{Request, Response};

use std::io::{BufRead, BufReader, Cursor, Write};
use std::net::TcpStream;
use std::time::{Instant, Duration};

/// Simple bitcoind JSON RPC client that implements the necessary subset of HTTP
#[derive(Copy, Clone, Debug)]
pub struct SimpleBitcoindClient {
    default_port: u16,
    timeout: Duration,
}

/// Builder for non-standard `SimpleBitcoinClient`s
#[derive(Clone, Debug)]
pub struct SimpleBitcoindClientBuilder {
    client: SimpleBitcoindClient,
}

impl Default for SimpleBitcoindClient {
    fn default() -> Self {
        SimpleBitcoindClient {
            default_port: 8332,
            timeout: Duration::from_secs(15),
        }
    }
}


impl SimpleBitcoindClientBuilder {
    /// Construct new `SimpleBitcoinClientBuilder`
    pub fn new() -> SimpleBitcoindClientBuilder {
        SimpleBitcoindClientBuilder {
            client: SimpleBitcoindClient::new(),
        }
    }

    /// Sets the port that the client will connect to in case none was specified in the URL of the
    /// request.
    pub fn default_port(mut self, port: u16) -> Self {
        self.client.default_port = port;
        self
    }

    /// Sets the timeout after which requests will abort if they aren't finished
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.client.timeout = timeout;
        self
    }

    /// Builds the final `SimpleBitcoindClient`
    pub fn build(self) -> SimpleBitcoindClient {
        self.client
    }
}

impl SimpleBitcoindClient {
    /// Construct a new `SimpleBitcoindClient` with default parameters
    pub fn new() -> Self {
        SimpleBitcoindClient::default()
    }

    /// Returns a builder for `SimpleBitcoindClient`
    pub fn builder() -> SimpleBitcoindClientBuilder {
        SimpleBitcoindClientBuilder::new()
    }
}

/// Try to read a line from a buffered reader. If no line can be read till the deadline is reached
/// return a timeout error.
fn get_line<R: BufRead>(reader: &mut R, deadline: Instant) -> Result<String, Error> {
    let mut line = String::new();
    while deadline > Instant::now() {
        match reader.read_line(&mut line) {
            // EOF reached for now, try again later
            Ok(0) => std::thread::yield_now(),
            // Socket would block, try again later
            Err(ref se) if se.kind() == std::io::ErrorKind::WouldBlock => std::thread::yield_now(),
            // received useful data, return it
            Ok(_) => return Ok(line),
            // io error occurred, abort
            Err(e) => return Err(Error::SocketError(e)),
        }
    }
    Err(Error::Timeout)
}

impl HttpRoundTripper for SimpleBitcoindClient {
    type ResponseBody = Cursor<Vec<u8>>;
    type Err = Error;

    fn request(&self, request: Request<&[u8]>) -> Result<Response<Self::ResponseBody>, Self::Err> {
        // Parse request
        let server = match request
            .uri()
            .authority_part()
            .map(|authority|{
                (
                    authority.host(),
                    authority.port_part().map(|p| p.as_u16()).unwrap_or(self.default_port)
                )
            }) {
            Some(s) => s,
            None => return Err(Error::NoHost),
        };
        let method = request.method();
        let uri = request.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");

        // Open connection
        let request_deadline = Instant::now() + self.timeout;
        let mut sock = TcpStream::connect(server)?;
        sock.set_nonblocking(true)?;

        // Send HTTP request
        sock.write_all(format!("{} {} HTTP/1.0\r\n", method, uri).as_bytes())?;
        sock.write_all("Content-Type: application/json\r\n".as_bytes())?;
        sock.write_all(format!("Content-Length: {}\r\n", request.body().len()).as_bytes())?;
        for (key, value) in request.headers() {
            sock.write_all(key.as_ref())?;
            sock.write_all(": ".as_bytes())?;
            sock.write_all(value.as_ref())?;
            sock.write_all("\r\n".as_bytes())?;
        }
        sock.write_all("\r\n".as_bytes())?;
        sock.write_all(request.body())?;

        // Receive response
        let mut reader = BufReader::new(sock);

        // Parse first HTTP response header line
        let http_response = get_line(&mut reader, request_deadline)?;
        if http_response.len() < 12 || !http_response.starts_with("HTTP/1.0 ") {
            return Err(Error::HttpParseError);
        }
        match http_response[9..12].parse::<u16>() {
            Ok(200) => {},
            Ok(e) => return Err(Error::ErrorCode(e)),
            Err(_) => return Err(Error::HttpParseError),
        };

        // Skip response header fields
        while get_line(&mut reader, request_deadline)? != "\r\n" {}

        // Read and return actual response line
        get_line(&mut reader, request_deadline)
            .map(|response| Response::new(Cursor::new(response.into_bytes())))
    }
}

/// Error that can happen when sending requests
#[derive(Debug)]
pub enum Error {
    /// The request didn't specify a host to connect to
    NoHost,
    /// An error occurred on the socket layer
    SocketError(std::io::Error),
    /// The HTTP header of the response couldn't be parsed
    HttpParseError,
    /// The server responded with a non-200 HTTP code
    ErrorCode(u16),
    /// We didn't receive a complete response till the deadline ran out
    Timeout,
}

impl PartialEq for Error {
    fn eq(&self, other: &Error) -> bool {
        match (self, other) {
            (Error::NoHost, Error::NoHost) => true,
            (Error::SocketError(s), Error::SocketError(o)) => s.kind() == o.kind(),
            (Error::HttpParseError, Error::HttpParseError) => true,
            (Error::ErrorCode(s), Error::ErrorCode(o)) => s == o,
            (Error::Timeout, Error::Timeout) => true,
            _ => false,
        }
    }
}

impl std::error::Error for Error {
    fn description(&self) -> &'static str {
        match *self {
            Error::NoHost => "No host was given in the URL.",
            Error::SocketError(_) => "Couldn't connect to given host.",
            Error::HttpParseError => "Couldn't parse HTTP response header.",
            Error::ErrorCode(_) => "Received HTTP error.",
            Error::Timeout => "Didn't receive response data in time, timed out.",
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        match *self {
            Error::NoHost => f.write_str("No host was given in the URL."),
            Error::SocketError(ref e) => write!(f, "Couldn't connect to host: {}", e),
            Error::HttpParseError => f.write_str("Couldn't parse response header."),
            Error::ErrorCode(e) => write!(f, "HTTP error {}", e),
            Error::Timeout => f.write_str("Didn't receive response data in time, timed out."),
        }.expect("writing the error message should work");

        Ok(())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::SocketError(e)
    }
}


#[cfg(test)]
mod tests {
    use ::{Error, SimpleBitcoindClient};
    use http::Request;
    use jsonrpc::client::HttpRoundTripper;
    use std::io::{BufRead, BufReader, BufWriter, ErrorKind, Write};
    use std::net::{Shutdown, TcpListener};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    fn simple_bitcoind_like_http_server(running: Arc<AtomicBool>) {
        println!("starting test server");
        let listener = TcpListener::bind("127.0.0.1:8332").unwrap();
        listener.set_nonblocking(true).unwrap();

        'server: while running.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok(connection) => {
                    let mut writer = BufWriter::new(connection.0.try_clone().unwrap());
                    let mut reader = BufReader::new(connection.0.try_clone().unwrap());

                    let req_header = 'connection: loop {
                        let mut line = String::new();
                        match reader.read_line(&mut line) {
                            Ok(0) => thread::yield_now(),
                            Ok(_) => break 'connection line,
                            Err(e) => {
                                eprintln!("Connection closed unexpectedly: {}", e);
                                continue 'server
                            },
                        }
                    };

                    let response = match req_header.as_str() {
                        "POST /invalid-header HTTP/1.0\r\n" => "Test\r\n",
                        "POST /stall HTTP/1.0\r\n" => {
                            thread::sleep(Duration::from_secs(2));
                            ""
                        },
                        "POST /server-error HTTP/1.0\r\n" => "HTTP/1.0 500 Error\r\n\r\n",
                        "POST /close HTTP/1.0\r\n" => "",
                        r => panic!("Unexpected request: {}", r),
                    };

                    writer.write_all(response.as_bytes()).unwrap();
                    writer.flush().unwrap();
                },
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    thread::yield_now();
                },
                Err(e) => {
                    panic!("The server crashed: {}", e);
                }
            }
        }
        println!("stopping test server")
    }

    #[test]
    fn tets_with_local_server() {
        let run_server = Arc::new(AtomicBool::new(true));
        let run_server_t = run_server.clone();
        let server_thread = thread::spawn(move || simple_bitcoind_like_http_server(run_server_t));
        thread::sleep(Duration::from_secs(1));

        let client = SimpleBitcoindClient::builder()
            .timeout(Duration::from_secs(1))
            .build();

        // Test invalid response header
        let request = Request::builder()
            .uri("http://127.0.0.1/invalid-header")
            .method("POST")
            .body("{}".as_bytes())
            .unwrap();
        assert_eq!(client.request(request).unwrap_err(), Error::HttpParseError);

        // Test timeout
        let request = Request::builder()
            .uri("http://127.0.0.1/stall")
            .method("POST")
            .body("{}".as_bytes())
            .unwrap();
        assert_eq!(client.request(request).unwrap_err(), Error::Timeout);
        thread::sleep(Duration::from_secs(2));

        // Test server error
        let request = Request::builder()
            .uri("http://127.0.0.1/server-error")
            .method("POST")
            .body("{}".as_bytes())
            .unwrap();
        assert_eq!(client.request(request).unwrap_err(), Error::ErrorCode(500));

        // Test server error
        let request = Request::builder()
            .uri("http://127.0.0.1/close")
            .method("POST")
            .body("{}".as_bytes())
            .unwrap();
        // Can be timeout or connection reset, depending on the system
        assert!(client.request(request).is_err());

        run_server.store(false, Ordering::Relaxed);
        server_thread.join().unwrap();
    }
}
