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

/// `http::client` reads the proxy variables once, on first use. The parent
/// serves the proxy, because the child's variables name its port before the
/// child starts.
#[test]
fn a_host_no_proxy_names_is_reached_directly_and_others_through_the_proxy() {
    let proxy = serve("proxied");
    devkit_git::child_env::child_env()
        .set("HTTP_PROXY", format!("http://127.0.0.1:{proxy}"))
        .set("NO_PROXY", "127.0.0.1")
        .run(|| {
            let origin = serve("direct");
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
        });
}
