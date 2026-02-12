// main.rs
use crate::router::Router;
use std::io::Result;
use std::net::TcpListener;

mod node;
mod response;
mod router;
mod routes;

fn main() -> Result<()> {
    let port = std::env::var("PORT").unwrap_or(String::from("7000"));
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr)?;

    let pid = std::process::id();
    println!("[{}] server listening on {}", pid, addr);

    let mut router = Router::new();
    routes::configure(&mut router);

    for client in listener.incoming() {
        let client = client?;
        let addr = client.peer_addr()?;
        println!("[{pid}] client connected at {addr}");
        router.route_client(client)?;
    }

    Ok(())
}
