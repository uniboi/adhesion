use std::{
    collections::HashMap,
    io::{prelude::*, BufReader},
    net::{TcpListener, TcpStream},
    sync::Arc,
};

use crate::thread_pool::ThreadPool;

pub type HTTPListener<T> = fn(
    &HashMap<&str, &str>, /* headers */
    &String,              /* body */
    &HashMap<&str, &str>, /* query params */
    &T,
) -> HTTPResponse;

pub struct HTTPServer<T: Clone + std::marker::Sync + std::marker::Send + 'static> {
    pub address: String,
    pub port: u64,
    pub listeners: Arc<HashMap<Route, HTTPListener<T>>>,
    pub default_404_listener: Arc<Option<HTTPListener<T>>>,
    pub threads: usize,
    pub passthrough: T,
}

pub struct HTTPStatus {
    pub status: u16,
    pub reason: String,
}

pub struct HTTPResponse {
    pub status: HTTPStatus,
    pub body: String,
    pub headers: HashMap<String, String>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Route {
    pub method: HTTPMethod,
    pub location: String,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HTTPMethod {
    GET,
    HEAD,
    POST,
    PUT,
    DELETE,
    CONNECT,
    OPTION,
    TRACE,
    PATCH,
    INVALID,
}

impl<T: Clone + std::marker::Sync + std::marker::Send + 'static> HTTPServer<T> {
    pub fn listen(&self) {
        let listener = TcpListener::bind(format!("{}:{}", self.address, self.port))
            .expect("failed binding to socket!");
        let pool = ThreadPool::new(self.threads);

        println!("listening on http://{}:{}", self.address, self.port);

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let cloned_listeners = Arc::clone(&self.listeners);
                    let cloned_404_handler = Arc::clone(&self.default_404_listener);
                    let pt = self.passthrough.clone();
                    pool.execute(move || {
                        HTTPServer::<T>::handle_stream(
                            &stream,
                            cloned_listeners,
                            cloned_404_handler,
                            &pt,
                        )
                    });
                }
                Err(error) => println!("connection dropped because of error: {}", error),
            }
        }
    }

    fn handle_stream(
        stream: &TcpStream,
        listeners: Arc<HashMap<Route, HTTPListener<T>>>,
        default_404_handler: Arc<Option<HTTPListener<T>>>,
        passthrough: &T,
    ) {
        let mut reader = BufReader::new(stream);
        let mut request = String::new(); // string to be fed bytes of the stream

        loop {
            let size: usize;
            match reader.read_line(&mut request) {
                Ok(line) => size = line,
                Err(error) => {
                    println!("fatal error reading request stream: {}", error);
                    HTTPServer::<T>::send_400_default_response(stream); // TODO: test if response is being sent
                    return;
                }
            }
            if size < 3 {
                //detect empty line
                break;
            }
        }

        let mut content_size = 0;
        let lines: Vec<&str> = request.split("\n").collect();

        if lines.len() < 3 {
            HTTPServer::<T>::send_400_default_response(stream);
            return;
        }

        let mut headers: HashMap<&str, &str> = HashMap::new();

        for l in &lines[1..] {
            let pair: Vec<&str> = l.split(":").collect();
            if pair.len() == 2 {
                headers.insert(pair[0], pair[1].trim());

                if l.starts_with("Content-Length") {
                    content_size = match pair[1].trim().parse::<usize>() {
                        Ok(size) => size,
                        Err(_err) => 0, // in case of invalid data, ignore the contents
                    }; // Get Content-Length
                }
            }
        }

        let context: Vec<&str> = lines[0].split(" ").collect();
        if context.len() < 3 {
            HTTPServer::<T>::send_400_default_response(stream);
            return;
        }

        let mut content_buffer = vec![0; content_size]; //New Vector with size of Content
        reader.read_exact(&mut content_buffer).unwrap(); //Get the Body Content.

        let query_index = match context[1].find("?") {
            Some(x) => x,
            None => context[1].len(),
        };

        let location = &context[1][..query_index];
        let query = &context[1][query_index..];

        let mut query_params: HashMap<&str, &str> = HashMap::new();
        for param in (if query.len() != 0 { &query[1..] } else { query }).split("&") {
            let arms: Vec<&str> = param.split("=").collect();
            if arms.len() == 2 {
                query_params.insert(arms[0], arms[1]);
            }
        }

        println!(
            "full: {}, {:?}, {:?}, {}",
            context[1],
            location,
            query_params,
            stream.local_addr().unwrap()
        );

        let body = String::from_utf8(content_buffer).unwrap_or_else(|err| {
            println!(
                "failed parsing utf8 body because of error: {err}. Defaulting to empty string."
            );
            String::from("")
        });

        let mut trimmed_location = location;

        while trimmed_location.ends_with("/") && trimmed_location.len() > 1 {
            trimmed_location = &location[..trimmed_location.len() - 1];
        }

        let response = match listeners.get(&Route {
            method: match context[0] {
                "GET" => HTTPMethod::GET,
                "HEAD" => HTTPMethod::HEAD,
                "POST" => HTTPMethod::POST,
                "PUT" => HTTPMethod::PUT,
                "DELETE" => HTTPMethod::DELETE,
                "CONNECT" => HTTPMethod::CONNECT,
                "OPTION" => HTTPMethod::OPTION,
                "TRACE" => HTTPMethod::TRACE,
                "PATCH" => HTTPMethod::PATCH,
                &_ => {
                    // end stream now
                    HTTPServer::<T>::send_400_default_response(stream);
                    HTTPMethod::INVALID
                }
            },
            location: String::from(trimmed_location),
        }) {
            Some(listener) => listener(&headers, &body, &query_params, passthrough),
            None => match *default_404_handler {
                Some(ref handler) => handler(&headers, &body, &query_params, passthrough),
                None => get_404_default_response(),
            },
        };

        // println!("{:#?}", headers);

        // for byte in content_buffer {
        //     println!("{}", byte as char);
        // }

        HTTPServer::<T>::close_stream(stream, &response)
    }

    fn close_stream(mut stream: &TcpStream, response: &HTTPResponse) {
        stream
            .write(
                format!(
                    "HTTP/1.1 {} {}\r\n{}\r\n{}",
                    response.status.status,
                    response.status.reason,
                    parse_headers(&response.headers),
                    response.body,
                )
                .as_bytes(),
            )
            .unwrap();
        stream.flush().unwrap();
    }

    fn send_400_default_response(stream: &TcpStream) {
        HTTPServer::<T>::close_stream(
            stream,
            &HTTPResponse {
                status: HTTPStatus::new(400),
                body: String::from("Received invalid data"),
                headers: HashMap::from([(
                    String::from("Content-Length"),
                    21.to_string(), /* 21 : length of string `Received invalid data` */
                )]),
            },
        );
    }
}

impl HTTPStatus {
    fn new(code: u16) -> HTTPStatus {
        HTTPStatus { status: code, reason: http_code_reason(code) }
    }
}

// http server internal utils

fn parse_headers(headers: &HashMap<String, String>) -> String {
    let mut converted: String = String::from("");
    for header in headers.iter() {
        converted.push_str(&format!("{}:{}\n", header.0, header.1));
    }
    return converted;
}

fn get_404_default_response() -> HTTPResponse {
    HTTPResponse {
        status: HTTPStatus::new(404),
        headers: HashMap::from([(
            String::from("Content-Length"),
            56.to_string(), /* 56 : length of string `The requested resource hasn't been found on this server.` */
        )]),
        body: String::from("The requested resource hasn't been found on this server."),
    }
}

// public utils

/// get a map with Content-Length prefilled
pub fn default_headers(content: &String) -> HashMap<String, String> {
    HashMap::from([(
        String::from("Content-Length"),
        content.len().to_owned().to_string(),
    )])
}

pub fn response_200(body: Option<String>) -> HTTPResponse {
    let body = match body {Some(b) => b, None => String::from("")};
    HTTPResponse { status: HTTPStatus::new(200), headers: default_headers(&body), body }
}

pub fn http_code_reason(code: u16) -> String {
    let r: Option<&str> = match code {
        100 => Some("Continue"),
        101 => Some("Switching Protocols"),
        103 => Some("Early Hints"),
        200 => Some("OK"),
        201 => Some("Created"),
        202 => Some("Accepted"),
        203 => Some("Non-Authoritative Information"),
        204 => Some("No Content"),
        205 => Some("Reset Content"),
        206 => Some("Partial Content"),
        300 => Some("Multiple Choices"),
        301 => Some("Moved Permanently"),
        302 => Some("Found"),
        303 => Some("See Other"),
        304 => Some("Not Modigied"),
        307 => Some("Temporary Redirect"),
        308 => Some("Permanent Redirect"),
        400 => Some("Bad Request"),
        401 => Some("Unauthorized"),
        402 => Some("Payment Required"),
        403 => Some("Forbidden"),
        404 => Some("Not Found"),
        405 => Some("Method not Allowed"),
        406 => Some("Not Acceptable"),
        407 => Some("Proxy Authentication Required"),
        408 => Some("Request Timeout"),
        409 => Some("Conflict"),
        410 => Some("Gone"),
        411 => Some("Length Required"),
        412 => Some("Precondition Failed"),
        413 => Some("Payload Too Large"),
        414 => Some("URI Too Long"),
        415 => Some("Unsupported Media Type"),
        416 => Some("Range not Satisfiable"),
        417 => Some("Expectation Failed"),
        418 => Some("I'm a teapot"),
        422 => Some("Unprocessable Entity"),
        425 => Some("Too Early"),
        426 => Some("Upgrade Required"),
        428 => Some("Precondition Required"),
        429 => Some("Too Many Requirests"),
        431 => Some("Request Header Fields Too Large"),
        451 => Some("Unavailable For Legal Reasons"),
        500 => Some("Internal Server Error"),
        501 => Some("Not Implemented"),
        502 => Some("Bad Gateway"),
        503 => Some("Service Unavailable"),
        504 => Some("Gateway Timeout"),
        505 => Some("HTTP Version Not Supported"),
        506 => Some("Variant Also Negotiates"),
        507 => Some("Insufficient Storage"),
        508 => Some("Loop Detected"),
        510 => Some("Not Extended"),
        511 => Some("Network Authentication Required"),
        _ => None,
    };
    String::from(r.expect("Invalid HTTP Status Code Provided"))
}
