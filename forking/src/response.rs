// response.rs
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Result, Write};
use std::net::TcpStream;

pub struct Response {
    writer: BufWriter<TcpStream>,
}

pub fn status(code: i32) -> &'static str {
    match code {
        200 => "OK",
        400 => "BAD REQUEST",
        404 => "NOT FOUND",
        _ => "NOT IMPLEMENTED",
    }
}

impl Response {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            writer: BufWriter::new(stream),
        }
    }

    pub fn send_file(mut self, code: i32, path: &str) -> Result<()> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut body = Vec::new();
        reader.read_to_end(&mut body)?;

        let content_type = format!("{}; charset=UTF-8", mime_type(path));

        write!(self.writer, "HTTP/1.1 {} {}\r\n", code, status(code))?;

        write!(self.writer, "Content-Length: {}\r\n", body.len())?;
        write!(self.writer, "Content-Type: {}\r\n", content_type)?;
        write!(self.writer, "\r\n")?;
        self.writer.write_all(&body)?;

        self.writer.flush()
    }
}

fn mime_type(path: &str) -> &str {
    match path.rsplit('.').next() {
        Some("html") => "text/html",
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("ico") => "image/x-icon",
        Some("pdf") => "application/pdf",
        _ => "text/plain",
    }
}
