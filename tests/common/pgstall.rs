//! A Postgres server that accepts a client's startup without a password and
//! then never answers a query, as a database stalled behind a live pooler
//! looks to a client.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
};

/// The server, listening until the test process ends.
pub struct Stalled {
    pub addr: String,
}

const SSL_REQUEST: u32 = 80_877_103;

impl Stalled {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let _ = serve(stream);
                });
            }
        });
        Self { addr }
    }
}

fn read_u32(stream: &mut TcpStream) -> std::io::Result<u32> {
    let mut buf = [0; 4];
    stream.read_exact(&mut buf)?;
    Ok(u32::from_be_bytes(buf))
}

/// Answers an SSL request with `N`, accepts the startup message, reports
/// authentication done and the connection ready, then reads and drops
/// everything the client sends.
fn serve(mut stream: TcpStream) -> std::io::Result<()> {
    let (mut len, mut code) = (read_u32(&mut stream)?, read_u32(&mut stream)?);
    if code == SSL_REQUEST {
        stream.write_all(b"N")?;
        (len, code) = (read_u32(&mut stream)?, read_u32(&mut stream)?);
    }
    let _ = code;
    let mut rest = vec![0; len as usize - 8];
    stream.read_exact(&mut rest)?;
    stream.write_all(&[b'R', 0, 0, 0, 8, 0, 0, 0, 0, b'Z', 0, 0, 0, 5, b'I'])?;
    let mut sink = [0; 1024];
    while stream.read(&mut sink)? > 0 {}
    Ok(())
}
