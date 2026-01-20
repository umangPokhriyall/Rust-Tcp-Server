use std::{
    fs::{self},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
};

// use server::ThreadPool;
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() {
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();

    let index_html = fs::read_to_string("server/index.html").unwrap();
    let not_found_html = fs::read_to_string("server/404.html").unwrap();

    let listner = TcpListener::bind("127.0.0.1:7878").unwrap();

    // let pool = ThreadPool::new(4);

    for stream in listner.incoming().take(8) {
        let stream = stream.unwrap();

        let index = index_html.clone();
        let not_found = not_found_html.clone();

        // pool.execute(move || {
        //     handle_connection(stream, &index, &not_found);
        // });
        handle_connection(stream, &index, &not_found);
    }
}

fn handle_connection(mut stream: TcpStream, index_html: &str, not_found_html: &str) {
    let mut bytes = [0; 1024];
    stream.read(&mut bytes).unwrap();

    // println!("Request {}", String::from_utf8_lossy(&bytes));
    // println!("Request {:?}", bytes)

    let get = b"GET / HTTP/1.1\r\n";

    let (status_line, file) = if bytes.starts_with(get) {
        ("HTTP/1.1 200 OK", index_html)
    } else {
        ("HTTP/1.1 404 NOT FOUND", not_found_html)
    };

    let response = format!(
        "{}\r\nContent-Length: {}\r\n\r\n{}",
        status_line,
        file.len(),
        file
    );

    stream.write(response.as_bytes()).unwrap();
    // stream.flush().unwrap();
}
