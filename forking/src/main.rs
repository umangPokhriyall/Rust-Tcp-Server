// main.rs
use crate::router::Router;
use std::io::Error;
use std::io::Result;
use std::net::TcpListener;

mod node;
mod response;
mod router;
mod routes;

fn check_err(num: i32) -> Result<i32> {
    if num < 0 {
        return Err(Error::last_os_error());
    }
    Ok(num)
}

fn fork() -> Result<u32> {
    check_err(unsafe { libc::fork() }).map(|pid| pid as u32)
}

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

        let child_pid = fork()?;

        if child_pid == 0 {
            router.route_client(client)?;
            break;
        } else {
            println!("[{pid}] forking process, new {child_pid}")
        }
    }

    Ok(())
}
