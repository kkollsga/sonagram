//! The production Last.fm client against a local one-shot HTTP server: the
//! success body parses, an HTTP error status yields no data, and a 200 body
//! carrying Last.fm's `error` member yields no data.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use sonagram::enrich::{LastfmApi, UreqClient};

/// Serve exactly one request with `status_line` and `body`; return the base URL
/// and the request line the client sent.
fn serve_once(
    status_line: &'static str,
    body: &'static str,
) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/2.0/", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap();
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        let response = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        request.lines().next().unwrap_or_default().to_string()
    });
    (base, handle)
}

#[test]
fn success_body_parses_and_the_query_carries_method_and_key() {
    let (base, server) = serve_once("200 OK", r#"{"artist":{"name":"Test"}}"#);
    let client = UreqClient::with_base("KEY123".into(), base);
    let json = client
        .call("artist.getinfo", &[("artist", "Test")])
        .expect("200 JSON body parses");
    assert_eq!(json["artist"]["name"], "Test");
    let request_line = server.join().unwrap();
    for needle in [
        "method=artist.getinfo",
        "api_key=KEY123",
        "format=json",
        "artist=Test",
    ] {
        assert!(
            request_line.contains(needle),
            "{needle} missing: {request_line}"
        );
    }
}

#[test]
fn http_error_status_yields_no_data() {
    let (base, server) = serve_once("500 Internal Server Error", r#"{"artist":{"name":"Test"}}"#);
    let client = UreqClient::with_base("KEY".into(), base);
    assert!(client.call("artist.getinfo", &[]).is_none());
    server.join().unwrap();
}

#[test]
fn lastfm_error_member_yields_no_data() {
    let (base, server) = serve_once("200 OK", r#"{"error":6,"message":"not found"}"#);
    let client = UreqClient::with_base("KEY".into(), base);
    assert!(client.call("artist.getinfo", &[]).is_none());
    server.join().unwrap();
}
