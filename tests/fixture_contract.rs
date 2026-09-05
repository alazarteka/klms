#![allow(dead_code)]

include!("fixture/server.rs");

mod tests {
    use super::*;
    use std::{net::Shutdown, panic::catch_unwind};

    fn connect(server: &Server) -> TcpStream {
        let stream = TcpStream::connect(server.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
    }

    fn response(stream: &mut TcpStream) -> String {
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn delayed_first_byte_is_accepted() {
        let server = Server::new(|_| Response::html("ok"));
        let mut stream = connect(&server);
        thread::sleep(Duration::from_millis(30));
        stream.write_all(b"GET /delayed HTTP/1.1\r\n\r\n").unwrap();
        assert!(response(&mut stream).ends_with("ok"));
        assert_eq!(server.requests(), ["GET /delayed HTTP/1.1"]);
    }

    #[test]
    fn split_headers_are_recorded_in_full() {
        let server = Server::new(|_| Response::html("ok"));
        let mut stream = connect(&server);
        stream
            .write_all(b"GET /split HTTP/1.1\r\nX-Test: par")
            .unwrap();
        thread::sleep(Duration::from_millis(30));
        assert!(server.requests().is_empty());
        stream.write_all(b"tial\r\n\r").unwrap();
        thread::sleep(Duration::from_millis(30));
        stream.write_all(b"\n").unwrap();
        assert!(response(&mut stream).ends_with("ok"));
        assert_eq!(server.recorded()[0].headers, ["X-Test: partial"]);
    }

    #[test]
    fn early_eof_and_oversize_headers_are_not_dispatched() {
        let server = Server::new(|_| panic!("incomplete request was dispatched"));
        for request in [
            b"GET /incomplete HTTP/1.1\r\n".to_vec(),
            vec![b'x'; 16 * 1024],
        ] {
            let mut stream = connect(&server);
            stream.write_all(&request).unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
            assert!(response(&mut stream).is_empty());
        }
        assert!(server.requests().is_empty());
    }

    #[test]
    fn stalled_headers_have_a_total_deadline() {
        let server = Server::new(|_| panic!("incomplete request was dispatched"));
        let mut stream = connect(&server);
        stream.write_all(b"GET /stalled HTTP/1.1\r\n").unwrap();
        // Keep sending bytes well inside the per-read timeout. A server that
        // resets its deadline on each read will outlive the client's generous
        // three-second timeout; a total deadline closes this connection.
        let mut writer = stream.try_clone().unwrap();
        let sending = thread::spawn(move || {
            for _ in 0..40 {
                thread::sleep(Duration::from_millis(100));
                if writer.write_all(b"x").is_err() {
                    break;
                }
            }
        });
        let received = response(&mut stream);
        sending.join().unwrap();
        assert!(received.is_empty());
        assert!(server.requests().is_empty());
    }

    fn failed_server() -> Server {
        Server {
            address: "127.0.0.1:1".parse().unwrap(),
            requests: Arc::new(Mutex::new(Vec::new())),
            stopped: Arc::new(AtomicBool::new(false)),
            thread: Some(thread::spawn(|| panic!("fixture failure"))),
        }
    }

    #[test]
    fn cleanup_preserves_fixture_failure_without_double_panic() {
        let failure = catch_unwind(|| drop(failed_server())).unwrap_err();
        assert_eq!(failure.downcast_ref::<&str>(), Some(&"fixture failure"));
        let failure = catch_unwind(|| {
            let _server = failed_server();
            panic!("test failure");
        })
        .unwrap_err();
        assert_eq!(failure.downcast_ref::<&str>(), Some(&"test failure"));
    }
}
