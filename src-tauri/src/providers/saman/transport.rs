//! Framed transports for the SSP1126 terminal (TCP + Serial).
//!
//! The framing is **asymmetric** — the easiest thing to get wrong:
//!
//! ```text
//! PC  -> POS:  [ISO-8583 message]                                        (raw, nothing added)
//! POS -> PC :  [2-byte BE length][5-byte header 60 00 00 00 00][ISO-8583 message]
//! ```
//!
//! On responses the length counts header + ISO bytes. We strip that envelope on
//! receive and add **nothing** on send. Prefixing a request with the response
//! envelope shifts every field by 7 bytes and the terminal answers `DE39=98`,
//! which looks deceptively like the customer pressed Cancel.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// Bytes the terminal prepends to each response: 2-byte length + `60 00 00 00 00`.
const RX_ENVELOPE_LEN: usize = 7;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("receive timeout after {0}ms")]
    Timeout(u128),
    #[error("not connected")]
    NotConnected,
    #[error("connect timeout")]
    ConnectTimeout,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// Shared framing + buffering for both media.
#[derive(Default)]
struct Framer {
    rx_buffer: Vec<u8>,
    /// Queue of complete inner ISO messages waiting to be consumed.
    inbox: VecDeque<Vec<u8>>,
}

impl Framer {
    fn feed(&mut self, data: &[u8]) {
        self.rx_buffer.extend_from_slice(data);
        loop {
            if self.rx_buffer.len() < 2 {
                return;
            }
            let msg_len = u16::from_be_bytes([self.rx_buffer[0], self.rx_buffer[1]]) as usize;
            let frame_len = msg_len + 2;
            if self.rx_buffer.len() < frame_len {
                return;
            }
            let frame: Vec<u8> = self.rx_buffer.drain(..frame_len).collect();
            if frame.len() >= RX_ENVELOPE_LEN {
                self.inbox.push_back(frame[RX_ENVELOPE_LEN..].to_vec());
            }
        }
    }

    fn next(&mut self) -> Option<Vec<u8>> {
        self.inbox.pop_front()
    }

    /// Mirrors the reference `ClearReadBuffer` before every write: drop stale
    /// completed responses and any partial frame from the previous step.
    fn clear(&mut self) {
        self.inbox.clear();
        self.rx_buffer.clear();
    }
}

#[async_trait]
pub trait Transport: Send {
    async fn connect(&mut self) -> Result<(), TransportError>;
    fn connected(&self) -> bool;
    async fn send(&mut self, iso_message: &[u8]) -> Result<(), TransportError>;
    /// Wait for one complete inner ISO message. A peer disconnect while waiting is
    /// surfaced as a timeout (matching the reference client, where a dead socket
    /// simply never delivers a frame) so the caller's decline-vs-timeout logic holds.
    async fn receive(&mut self, timeout: Duration) -> Result<Vec<u8>, TransportError>;
    fn clear_inbox(&mut self);
    fn close(&mut self);
}

// ---------------------------------------------------------------------- TCP

pub struct TcpTransport {
    host: String,
    port: u16,
    connect_timeout: Duration,
    stream: Option<TcpStream>,
    framer: Framer,
}

impl TcpTransport {
    pub fn new(host: &str, port: u16, connect_timeout: Duration) -> Self {
        Self {
            host: host.to_string(),
            port,
            connect_timeout,
            stream: None,
            framer: Framer::default(),
        }
    }
}

#[async_trait]
impl Transport for TcpTransport {
    async fn connect(&mut self) -> Result<(), TransportError> {
        let addr = format!("{}:{}", self.host, self.port);
        let stream = tokio::time::timeout(self.connect_timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| TransportError::ConnectTimeout)??;
        stream.set_nodelay(true).ok();
        self.stream = Some(stream);
        self.framer.clear();
        Ok(())
    }

    fn connected(&self) -> bool {
        self.stream.is_some()
    }

    async fn send(&mut self, iso_message: &[u8]) -> Result<(), TransportError> {
        let stream = self.stream.as_mut().ok_or(TransportError::NotConnected)?;
        stream.write_all(iso_message).await?;
        stream.flush().await?;
        Ok(())
    }

    async fn receive(&mut self, timeout: Duration) -> Result<Vec<u8>, TransportError> {
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 4096];
        loop {
            if let Some(frame) = self.framer.next() {
                return Ok(frame);
            }
            let Some(stream) = self.stream.as_mut() else {
                // Peer went away earlier in this wait: park until the deadline so the
                // caller sees a timeout, not a hard error (see trait docs).
                tokio::time::sleep_until(deadline).await;
                return Err(TransportError::Timeout(timeout.as_millis()));
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::Timeout(timeout.as_millis()));
            }
            match tokio::time::timeout(remaining, stream.read(&mut buf)).await {
                Err(_) => return Err(TransportError::Timeout(timeout.as_millis())),
                Ok(Ok(0)) => {
                    // EOF — connection closed by the terminal.
                    self.stream = None;
                }
                Ok(Ok(n)) => self.framer.feed(&buf[..n]),
                Ok(Err(_)) => {
                    self.stream = None;
                }
            }
        }
    }

    fn clear_inbox(&mut self) {
        self.framer.clear();
        // Also drain anything already sitting in the kernel buffer, so a stale frame
        // from the previous step can't be misread as the next step's response.
        if let Some(stream) = self.stream.as_mut() {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stream.try_read(&mut buf) {
                if n == 0 {
                    self.stream = None;
                    break;
                }
            }
        }
    }

    fn close(&mut self) {
        self.framer.clear();
        // Dropping the stream closes the fd; the OS flushes pending writes and
        // handles the FIN handshake — no event loop to keep alive here.
        self.stream = None;
    }
}

// -------------------------------------------------------------------- Serial

/// Serial channel: 8 data bits, 1 stop bit, no parity (fixed), default baud 19200.
///
/// The `serialport` crate is blocking, so a dedicated reader thread pumps bytes
/// into an mpsc channel (the same event-driven shape as the reference library).
pub struct SerialTransport {
    path: String,
    baud_rate: u32,
    connect_timeout: Duration,
    port: Option<Box<dyn serialport::SerialPort>>,
    rx: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    framer: Framer,
}

impl SerialTransport {
    pub fn new(path: &str, baud_rate: u32, connect_timeout: Duration) -> Self {
        Self {
            path: path.to_string(),
            baud_rate,
            connect_timeout,
            port: None,
            rx: None,
            stop: Arc::new(AtomicBool::new(false)),
            framer: Framer::default(),
        }
    }
}

#[async_trait]
impl Transport for SerialTransport {
    async fn connect(&mut self) -> Result<(), TransportError> {
        let path = self.path.clone();
        let baud = self.baud_rate;
        let timeout = self.connect_timeout;
        let port = tokio::task::spawn_blocking(move || {
            serialport::new(&path, baud)
                .data_bits(serialport::DataBits::Eight)
                .stop_bits(serialport::StopBits::One)
                .parity(serialport::Parity::None)
                .timeout(Duration::from_millis(100))
                .open()
        })
        .await
        .map_err(|e| TransportError::Other(e.to_string()))?
        .map_err(|e| TransportError::Other(format!("serial open failed: {e}")))?;
        let _ = timeout; // open is immediate for serial devices; kept for config parity

        let mut reader = port
            .try_clone()
            .map_err(|e| TransportError::Other(format!("serial clone failed: {e}")))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_r = stop.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while !stop_r.load(Ordering::Relaxed) {
                match reader.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(_) => break,
                }
            }
        });

        self.port = Some(port);
        self.rx = Some(rx);
        self.stop = stop;
        self.framer.clear();
        Ok(())
    }

    fn connected(&self) -> bool {
        self.port.is_some()
    }

    async fn send(&mut self, iso_message: &[u8]) -> Result<(), TransportError> {
        let port = self.port.as_mut().ok_or(TransportError::NotConnected)?;
        // Write + flush so the bytes hit the line before we resolve, matching the
        // reference overlapped WriteFile that waits for completion.
        port.write_all(iso_message)
            .and_then(|_| port.flush())
            .map_err(TransportError::Io)?;
        Ok(())
    }

    async fn receive(&mut self, timeout: Duration) -> Result<Vec<u8>, TransportError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = self.framer.next() {
                return Ok(frame);
            }
            let Some(rx) = self.rx.as_mut() else {
                tokio::time::sleep_until(deadline).await;
                return Err(TransportError::Timeout(timeout.as_millis()));
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::Timeout(timeout.as_millis()));
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Err(_) => return Err(TransportError::Timeout(timeout.as_millis())),
                Ok(Some(chunk)) => self.framer.feed(&chunk),
                Ok(None) => {
                    self.rx = None; // reader thread ended; park until deadline
                }
            }
        }
    }

    fn clear_inbox(&mut self) {
        self.framer.clear();
        if let Some(rx) = self.rx.as_mut() {
            while rx.try_recv().is_ok() {}
        }
    }

    fn close(&mut self) {
        self.framer.clear();
        self.stop.store(true, Ordering::Relaxed);
        self.rx = None;
        self.port = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framer_strips_envelope_and_buffers_partials() {
        let mut f = Framer::default();
        // Frame: len(header+iso)=5+3=8, header, iso = [0x03, 0x00, 0x99]
        let full = [0x00, 0x08, 0x60, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x99];
        f.feed(&full[..4]); // partial
        assert!(f.next().is_none());
        f.feed(&full[4..]);
        assert_eq!(f.next().unwrap(), vec![0x03, 0x00, 0x99]);
    }

    #[test]
    fn framer_handles_two_frames_in_one_chunk() {
        let mut f = Framer::default();
        let one = [0x00, 0x06, 0x60, 0x00, 0x00, 0x00, 0x00, 0xAA];
        let mut both = one.to_vec();
        both.extend_from_slice(&one);
        f.feed(&both);
        assert_eq!(f.next().unwrap(), vec![0xAA]);
        assert_eq!(f.next().unwrap(), vec![0xAA]);
        assert!(f.next().is_none());
    }
}
