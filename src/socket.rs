use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// Connect, write, or read failed — server down, restarting, or unreachable.
    Io,
    /// The pane is gone. Terminal: there is nothing left to report to.
    PaneNotFound,
}

/// Sends one request and reads the single response. Herdr closes the connection
/// after replying, so each call is a fresh connect. That is cheap on a local
/// Unix socket and removes any reconnect logic across server restarts.
pub fn send(socket_path: &Path, id: &str, method: &str, params: Value) -> Result<(), SendError> {
    let mut stream = UnixStream::connect(socket_path).map_err(|_| SendError::Io)?;
    stream.set_read_timeout(Some(TIMEOUT)).map_err(|_| SendError::Io)?;
    stream.set_write_timeout(Some(TIMEOUT)).map_err(|_| SendError::Io)?;

    let line = crate::proto::envelope(id, method, params);
    stream.write_all(line.as_bytes()).map_err(|_| SendError::Io)?;
    stream.flush().map_err(|_| SendError::Io)?;

    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(|_| SendError::Io)?;

    let parsed: Value = serde_json::from_str(response.trim()).map_err(|_| SendError::Io)?;
    match parsed.get("error") {
        None => Ok(()),
        Some(err) => {
            if err.get("code").and_then(Value::as_str) == Some("pane_not_found") {
                Err(SendError::PaneNotFound)
            } else {
                Err(SendError::Io)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    /// Stands in for Herdr: accepts one connection, hands back the request line,
    /// replies, then closes — mirroring the real server's one-shot behavior.
    fn fake_server(reply: &'static str) -> (tempfile::TempDir, std::path::PathBuf, mpsc::Receiver<String>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("herdr.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let _ = tx.send(line);
                let mut stream = reader.into_inner();
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        (dir, path, rx)
    }

    #[test]
    fn send_writes_one_framed_request_line() {
        let (_dir, path, rx) = fake_server("{\"id\":\"1\",\"result\":{\"type\":\"ok\"}}\n");
        let params = serde_json::json!({"pane_id": "w1:p2"});
        let result = send(&path, "1", "pane.release_agent", params);
        assert!(result.is_ok());

        let line = rx.recv().unwrap();
        assert!(line.ends_with('\n'));
        let parsed: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["method"], "pane.release_agent");
        assert_eq!(parsed["params"]["pane_id"], "w1:p2");
    }

    #[test]
    fn send_reports_pane_not_found_distinctly() {
        let (_dir, path, _rx) = fake_server(
            "{\"error\":{\"code\":\"pane_not_found\",\"message\":\"gone\"},\"id\":\"1\"}\n",
        );
        let result = send(&path, "1", "pane.report_agent", serde_json::json!({}));
        assert_eq!(result, Err(SendError::PaneNotFound));
    }

    #[test]
    fn send_reports_io_error_when_the_socket_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.sock");
        let result = send(&missing, "1", "ping", serde_json::json!({}));
        assert_eq!(result, Err(SendError::Io));
    }

    #[test]
    fn send_treats_other_errors_as_io() {
        let (_dir, path, _rx) = fake_server(
            "{\"error\":{\"code\":\"something_else\",\"message\":\"nope\"},\"id\":\"1\"}\n",
        );
        let result = send(&path, "1", "pane.report_agent", serde_json::json!({}));
        assert_eq!(result, Err(SendError::Io));
    }
}
