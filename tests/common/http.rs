use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const SERVER_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub struct BackgroundTcpServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl BackgroundTcpServer {
    pub fn start<F>(label: &str, run: F) -> Self
    where
        F: FnOnce(TcpListener, Arc<AtomicBool>) + Send + 'static,
    {
        let listener =
            TcpListener::bind("127.0.0.1:0").unwrap_or_else(|err| panic!("bind {label}: {err}"));
        listener
            .set_nonblocking(true)
            .unwrap_or_else(|err| panic!("set {label} listener non-blocking: {err}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|err| panic!("read {label} listener addr: {err}"));

        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handle = thread::spawn(move || run(listener, stop_flag));

        Self {
            addr,
            stop,
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for BackgroundTcpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn run_fake_http_server<F>(listener: &TcpListener, stop: &AtomicBool, mut handle_connection: F)
where
    F: FnMut(&mut TcpStream),
{
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        match listener.accept() {
            Ok((mut stream, _addr)) => handle_connection(&mut stream),
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::Interrupted =>
            {
                thread::sleep(SERVER_POLL_INTERVAL);
            }
            Err(_) => {
                // Keep the fake server alive across transient accept failures.
                // Shutdown is controlled via the explicit stop flag.
                thread::sleep(SERVER_POLL_INTERVAL);
            }
        }
    }
}

pub fn read_http_request_head(stream: &mut TcpStream) -> Option<String> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));

    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];

    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read_len) => {
                buf.extend_from_slice(&chunk[..read_len]);
                if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
                if buf.len() >= 64 * 1024 {
                    break;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }

    if buf.is_empty() {
        return None;
    }

    Some(String::from_utf8_lossy(&buf).into_owned())
}

pub fn write_http_response_text(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

pub fn write_http_response_bytes(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) {
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(headers.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}
