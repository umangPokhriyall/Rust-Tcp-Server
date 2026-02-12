// routes.rs
use crate::response::Response;
use crate::router::{Method, Router};
use std::io::Result;
use std::net::TcpStream;

pub fn configure(router: &mut Router) {
    router.insert(Method::GET, "/", index);
    router.insert(Method::GET, "/static/style.css", styles);
}

fn index(client: TcpStream) -> Result<()> {
    Response::new(client).send_file(200, "./static/index.html")
}

fn styles(client: TcpStream) -> Result<()> {
    Response::new(client).send_file(200, "./static/style.css")
}
