use crate::app::{ExitOutcome, SignalReq};
use nix::pty::{openpty, Winsize};
use nix::sys::signal::{kill, Signal};
use nix::unistd::{Pid, read};
use std::os::fd::AsFd;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Condvar};
use std::thread;
use std::time::Duration;

pub struct ByteLineSplitter {
    buffer: Vec<u8>,
}

impl ByteLineSplitter {
    #[must_use]
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    pub fn push_chunk(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(chunk);
        let mut lines = Vec::new();
        let mut search_start = 0;
        
        while let Some(newline_pos) = self.buffer[search_start..].iter().position(|&b| b == b'\n') {
            let absolute_pos = search_start + newline_pos;
            let end_idx = if absolute_pos > 0 && self.buffer[absolute_pos - 1] == b'\r' {
                absolute_pos - 1
            } else {
                absolute_pos
            };
            
            let line_bytes = &self.buffer[..end_idx];
            lines.push(String::from_utf8_lossy(line_bytes).into_owned());
            
            self.buffer.drain(..=absolute_pos);
            search_start = 0; // reset for new buffer length
        }
        
        lines
    }

    #[must_use]
    pub fn finish(mut self) -> Option<String> {
        if self.buffer.is_empty() {
            None
        } else {
            let s = String::from_utf8_lossy(&self.buffer).into_owned();
            self.buffer.clear();
            Some(s)
        }
    }
}

impl Default for ByteLineSplitter {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyEvent {
    Line(String),
    Exited(ExitOutcome),
}

pub struct PtyConfig {
    pub command: Vec<String>,
    pub inject_default_env: bool,
    pub cols: u16,
    pub rows: u16,
}

pub struct PtySession {
    child_pid: i32,
    master_fd: i32,
}

impl PtySession {
    #[must_use]
    pub fn child_pid(&self) -> i32 {
        self.child_pid
    }

    pub fn spawn(config: PtyConfig, tx: mpsc::Sender<PtyEvent>) -> Result<Self, String> {
        if config.command.is_empty() {
            return Err("Command is empty".to_string());
        }

        let winsize = Winsize {
            ws_row: config.rows,
            ws_col: config.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        let pty_res = openpty(Some(&winsize), None).map_err(|e| format!("Failed to openpty: {e}"))?;
        let master = pty_res.master;
        let slave = pty_res.slave;

        let mut cmd = Command::new(&config.command[0]);
        if config.command.len() > 1 {
            cmd.args(&config.command[1..]);
        }

        cmd.stdin(std::process::Stdio::from(slave.try_clone().unwrap()));
        cmd.stdout(std::process::Stdio::from(slave.try_clone().unwrap()));
        cmd.stderr(std::process::Stdio::from(slave.try_clone().unwrap()));

        if config.inject_default_env {
            let defaults = [
                ("OVERRIDE_LAUNCH_PROCESS_OUTPUT", "both"),
                ("RCUTILS_COLORIZED_OUTPUT", "1"),
                ("RCUTILS_LOGGING_BUFFERED_STREAM", "0"),
                ("PYTHONUNBUFFERED", "1"),
            ];
            for (k, v) in defaults {
                if std::env::var_os(k).is_none() {
                    cmd.env(k, v);
                }
            }
        }

        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                nix::libc::setsid();
                nix::libc::ioctl(0, nix::libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }

        let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn command: {e}"))?;
        let child_pid = child.id() as i32;

        drop(slave);

        let reader_done = Arc::new((Mutex::new(false), Condvar::new()));
        let reader_done_clone = Arc::clone(&reader_done);

        let master_fd = master.as_raw_fd();
        let master_owned = master;
        let tx_reader = tx.clone();
        
        thread::spawn(move || {
            let mut splitter = ByteLineSplitter::new();
            let mut buf = [0u8; 4096];
            loop {
                match read(master_owned.as_fd(), &mut buf) {
                    Ok(0) | Err(nix::errno::Errno::EIO) => {
                        if let Some(last_line) = splitter.finish() {
                            let _ = tx_reader.send(PtyEvent::Line(last_line));
                        }
                        break;
                    }
                    Ok(n) => {
                        let lines = splitter.push_chunk(&buf[..n]);
                        for line in lines {
                            if tx_reader.send(PtyEvent::Line(line)).is_err() {
                                break;
                            }
                        }
                    }
                    Err(_) => {
                        if let Some(last_line) = splitter.finish() {
                            let _ = tx_reader.send(PtyEvent::Line(last_line));
                        }
                        break;
                    }
                }
            }
            
            drop(master_owned);
            let (lock, cvar) = &*reader_done_clone;
            if let Ok(mut done) = lock.lock() {
                *done = true;
                cvar.notify_all();
            }
        });

        thread::spawn(move || {
            let outcome = match child.wait() {
                Ok(status) => {
                    if let Some(code) = status.code() {
                        ExitOutcome::Code(code)
                    } else if let Some(sig) = status.signal() {
                        ExitOutcome::Signal(sig)
                    } else {
                        ExitOutcome::Code(1)
                    }
                }
                Err(_) => ExitOutcome::Code(1),
            };
            
            let (lock, cvar) = &*reader_done;
            if let Ok(done) = lock.lock() {
                #[allow(clippy::collapsible_if)]
                if !*done {
                    let _ = cvar.wait_timeout(done, Duration::from_millis(100));
                }
            }
            
            let _ = tx.send(PtyEvent::Exited(outcome));
        });

        Ok(PtySession {
            child_pid,
            master_fd,
        })
    }

    pub fn send_signal(&self, sig: SignalReq) {
        let nix_sig = match sig {
            SignalReq::Int => Signal::SIGINT,
            SignalReq::Term => Signal::SIGTERM,
            SignalReq::Kill => Signal::SIGKILL,
        };
        let _ = kill(Pid::from_raw(-self.child_pid), nix_sig);
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            nix::libc::ioctl(self.master_fd, nix::libc::TIOCSWINSZ as _, &winsize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_splitter_preserves_multibyte_utf8_across_chunks_and_survives_invalid_bytes() {
        let mut splitter = ByteLineSplitter::new();
        
        let lines = splitter.push_chunk(&[0xff, b'a', b'\n']);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "\u{fffd}a");

        let msg = "🚀\n".as_bytes();
        assert_eq!(msg.len(), 5);
        let chunk1 = &msg[..2];
        let chunk2 = &msg[2..];
        
        let lines1 = splitter.push_chunk(chunk1);
        assert!(lines1.is_empty());
        let lines2 = splitter.push_chunk(chunk2);
        assert_eq!(lines2.len(), 1);
        assert_eq!(lines2[0], "🚀");

        let lines3 = splitter.push_chunk(b"hello\r\nworld\n");
        assert_eq!(lines3.len(), 2);
        assert_eq!(lines3[0], "hello");
        assert_eq!(lines3[1], "world");
    }
}
