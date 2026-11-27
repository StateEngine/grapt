//! A total socket allowance prevents a peer from renewing each I/O timeout.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

#[derive(Debug)]
pub(super) struct SocketWindow {
    pub(super) stream: TcpStream,
    pub(super) deadline: tokio::time::Instant,
}

impl SocketWindow {
    fn timeout(error: io::Error) -> io::Error {
        match error.kind() {
            // Blocking sockets report an elapsed SO_RCVTIMEO/SO_SNDTIMEO as
            // WouldBlock on Linux and TimedOut on Windows. Name the cause on
            // both, without changing the caller's after-send classification.
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => io::Error::new(
                io::ErrorKind::TimedOut,
                "socket acquisition allowance elapsed",
            ),
            _ => error,
        }
    }

    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(tokio::time::Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "socket acquisition allowance elapsed",
                )
            })
    }
}

impl Read for SocketWindow {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(bytes).map_err(Self::timeout)
    }
}

impl Write for SocketWindow {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(bytes).map_err(Self::timeout)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.remaining()?;
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn frc_expired_socket_allowance_refuses_read_and_write() -> Result<(), String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(1))
            .map_err(|error| error.to_string())?;
        let mut window = SocketWindow {
            stream,
            deadline: tokio::time::Instant::now() + Duration::from_millis(25),
        };
        let error = window
            .read(&mut [0; 1])
            .err()
            .ok_or("idle socket returned data")?;
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(error
            .to_string()
            .contains("socket acquisition allowance elapsed"));
        window.deadline = tokio::time::Instant::now();
        assert_eq!(
            window
                .write(b"must not be sent")
                .err()
                .map(|error| error.kind()),
            Some(io::ErrorKind::TimedOut)
        );
        Ok(())
    }
}
