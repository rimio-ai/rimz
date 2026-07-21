//! ttyd-shaped trace server for browser-access integration tests.

use std::env;
use std::fs::OpenOptions;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::time::Duration;

const INDEX: &str = "<html><head></head><body></body></html>";

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "--version") {
        writeln!(std::io::stdout().lock(), "ttyd version 1.7.7").expect("write version");
        return;
    }
    let log = env::var_os("RIMZ_TEST_TTYD_LOG").expect("RIMZ_TEST_TTYD_LOG unset");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .expect("open ttyd trace log");
    writeln!(file, "{}", args.join("\t")).expect("write ttyd trace");
    let port = args
        .windows(2)
        .find(|pair| pair[0] == "-p")
        .and_then(|pair| pair[1].parse::<u16>().ok())
        .expect("ttyd -p port");
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind ttyd trace port");
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { break };
        std::thread::spawn(move || {
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("set request timeout");
            let mut request = [0_u8; 8192];
            let Ok(read) = stream.read(&mut request) else {
                return;
            };
            if read == 0 || !request[..read].starts_with(b"GET ") {
                return;
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{INDEX}",
                INDEX.len()
            );
            let _ = stream.write_all(response.as_bytes());
        });
    }
}
