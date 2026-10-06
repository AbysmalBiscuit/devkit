//! A Postgres server that accepts a client's startup without a password and
//! then never answers a query, as a database stalled behind a live pooler
//! looks to a client.
//!
//! Tests elsewhere probe ports they freed, and one of those can be the port
//! this server took, so it counts only the connections whose startup names
//! its own database.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

/// The server, listening until the test process ends.
pub struct Stalled {
    pub addr: String,
    /// The database name no other server in any test process uses.
    #[allow(dead_code)]
    pub database: String,
    started: Arc<AtomicUsize>,
}

const SSL_REQUEST: u32 = 80_877_103;

impl Stalled {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let database = format!(
            "stalled_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        let started = Arc::new(AtomicUsize::new(0));
        let (counter, name) = (Arc::clone(&started), database.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (counter, name) = (Arc::clone(&counter), name.clone());
                std::thread::spawn(move || {
                    let _ = serve(stream, &name, &counter);
                });
            }
        });
        Self {
            addr,
            database,
            started,
        }
    }

    /// How many connections have started up naming this server's database.
    #[allow(dead_code)]
    pub fn connections(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }
}

fn read_u32(stream: &mut TcpStream) -> std::io::Result<u32> {
    let mut buf = [0; 4];
    stream.read_exact(&mut buf)?;
    Ok(u32::from_be_bytes(buf))
}

/// Answers an SSL request with `N`, accepts the startup message, counting it
/// in `started` when it names `database`, reports authentication done and
/// the connection ready, then reads and drops everything the client sends.
fn serve(mut stream: TcpStream, database: &str, started: &AtomicUsize) -> std::io::Result<()> {
    let mut len = read_u32(&mut stream)?;
    if read_u32(&mut stream)? == SSL_REQUEST {
        stream.write_all(b"N")?;
        len = read_u32(&mut stream)?;
        read_u32(&mut stream)?;
    }
    let mut rest = vec![0; len as usize - 8];
    stream.read_exact(&mut rest)?;
    let mut params = rest.split(|&b| b == 0);
    while let (Some(key), Some(value)) = (params.next(), params.next()) {
        if key == b"database" && value == database.as_bytes() {
            started.fetch_add(1, Ordering::SeqCst);
        }
    }
    stream.write_all(&[b'R', 0, 0, 0, 8, 0, 0, 0, 0, b'Z', 0, 0, 0, 5, b'I'])?;
    let mut sink = [0; 1024];
    while stream.read(&mut sink)? > 0 {}
    Ok(())
}
