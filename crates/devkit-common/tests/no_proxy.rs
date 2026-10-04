use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
};

/// A loopback HTTP server that answers every request with `body`; returns
/// its port.
fn serve(body: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut tcp in listener.incoming().flatten() {
            let mut reader = BufReader::new(&mut tcp);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let _ = write!(
                tcp,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

/// One sequential test: the proxy variables are process-global and
/// `http::client` reads them once, on first use.
#[test]
fn a_host_no_proxy_names_is_reached_directly_and_others_through_the_proxy() {
    let origin = serve("direct");
    let proxy = serve("proxied");
    unsafe {
        std::env::set_var("HTTP_PROXY", format!("http://127.0.0.1:{proxy}"));
        std::env::set_var("NO_PROXY", "127.0.0.1");
    }
    let get = |host: &str| {
        devkit_common::http::client()
            .get(format!("http://{host}:{origin}/"))
            .send()
            .unwrap()
            .text()
            .unwrap()
    };
    assert_eq!(get("127.0.0.1"), "direct");
    assert_eq!(get("localhost"), "proxied");
}
