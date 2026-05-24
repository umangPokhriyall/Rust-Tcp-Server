// router.rs
use crate::node::Node;
use crate::response::Response;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Result};
use std::net::TcpStream;

#[derive(PartialEq, Eq, Hash)]
pub enum Method {
    GET,
}

pub type HandlerFn = fn(TcpStream) -> Result<()>;

pub struct Router {
    routes: HashMap<Method, Node>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            routes: HashMap::new(),
        }
    }

    pub fn insert(&mut self, method: Method, path: &str, handler: HandlerFn) {
        let root = self.routes.entry(method).or_insert(Node::new(""));
        root.insert(path, handler);
        println!("Inserted route {}", path);
    }

    pub fn route_client(&self, mut client: TcpStream) -> Result<()> {
        let mut reader = BufReader::new(&client);
        let mut request_line = String::new();

        reader.read_line(&mut request_line)?;

        if request_line.is_empty() {
            return Ok(());
        }

        let addr = client.peer_addr()?;
        println!("{} @{addr} sent", format!("[{}]", std::process::id()));

        println!("Request: {}", request_line.trim());

        let parts: Vec<&str> = request_line.split_whitespace().collect();

        if parts.len() < 2 {
            return Response::new(client).send_file(400, "static/_400.html");
        }

        let method = match parts[0] {
            "GET" => Method::GET,
            _ => return Response::new(client).send_file(400, "static/_400.html"),
        };

        let path = parts[1];

        self.handle(method, path, client)
    }

    fn handle(&self, method: Method, path: &str, client: TcpStream) -> Result<()> {
        if let Some(root) = self.routes.get(&method) {
            if let Some(handler) = root.get(path) {
                return handler(client);
            }
        }

        Response::new(client).send_file(404, "static/not_found.html")
    }
}
