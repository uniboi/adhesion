use std::{
    collections::HashMap,
    io::{prelude::*, BufReader},
    net::{TcpListener, TcpStream},
    sync::Arc,
};

use crate::thread_pool::ThreadPool;

pub type HTTPListener = fn(
    &HashMap<&str, &str>, /* headers */
    &String,              /* body */
    &HashMap<&str, &str>, /* query params */
) -> HTTPResponse;

pub struct HTTPServer {
    pub address: String,
    pub port: u64,
    pub listeners: Arc<HashMap<Route, HTTPListener>>,
    pub default_404_listener: Arc<Option<HTTPListener>>,
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

impl HTTPServer {
    pub fn listen(&self) {
        let listener = TcpListener::bind(format!("{}:{}", self.address, self.port))
            .expect("failed binding to socket!");
        let pool = ThreadPool::new(5);

        println!("listening on http://{}:{}", self.address, self.port);

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let ref_counted_listeners = Arc::clone(&self.listeners);
                    let ref_counted_404_handler = Arc::clone(&self.default_404_listener);
                    pool.execute(move || {
                        HTTPServer::handle_stream(
                            &stream,
                            ref_counted_listeners,
                            ref_counted_404_handler,
                        )
                    });
                }
                Err(error) => println!("connection dropped because of error: {}", error),
            }
        }
    }

    fn handle_stream(
        stream: &TcpStream,
        listeners: Arc<HashMap<Route, HTTPListener>>,
        default_404_handler: Arc<Option<HTTPListener>>,
    ) {
        let mut reader = BufReader::new(stream);
        let mut request = String::new(); // string to be fed bytes of the stream

        loop {
            let size: usize;
            match reader.read_line(&mut request) {
                Ok(line) => size = line,
                Err(error) => {
                    println!("fatal error reading request stream: {}", error);
                    send_400_default_response(&stream); // TODO: test if response is being sent
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
            send_400_default_response(stream);
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
            send_400_default_response(stream);
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
                    send_400_default_response(stream);
                    HTTPMethod::INVALID
                }
            },
            location: String::from(trimmed_location),
        }) {
            Some(listener) => listener(&headers, &body, &query_params),
            None => match *default_404_handler {
                Some(ref handler) => handler(&headers, &body, &query_params),
                None => get_404_default_response(),
            },
        };

        // println!("{:#?}", headers);

        // for byte in content_buffer {
        //     println!("{}", byte as char);
        // }

        HTTPServer::close_stream(stream, &response)
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
}

fn parse_headers(headers: &HashMap<String, String>) -> String {
    let mut converted: String = String::from("");
    for header in headers.iter() {
        converted.push_str(&format!("{}:{}\n", header.0, header.1));
    }
    return converted;
}

fn get_404_default_response() -> HTTPResponse {
    HTTPResponse {
        status: HTTPStatus {
            status: 404,
            reason: String::from("Not Found"),
        },
        headers: HashMap::from([(
            String::from("Content-Length"),
            56.to_string(), /* 56 : length of string `The requested resource hasn't been found on this server.` */
        )]),
        body: String::from("The requested resource hasn't been found on this server."),
    }
}

fn send_400_default_response(stream: &TcpStream) {
    HTTPServer::close_stream(
        stream,
        &HTTPResponse {
            status: HTTPStatus {
                status: 400,
                reason: String::from("Bad Request"),
            },
            body: String::from("Received invalid data"),
            headers: HashMap::from([(
                String::from("Content-Length"),
                21.to_string(), /* 21 : length of string `Received invalid data` */
            )]),
        },
    );
}

/// get a map with Content-Length prefilled
pub fn default_headers(content: &String) -> HashMap<String, String> {
    HashMap::from([(
        String::from("Content-Length"),
        content.len().to_owned().to_string(),
    )])
}
