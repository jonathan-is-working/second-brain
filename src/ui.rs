//! Terminal feedback: an animated spinner on stderr for long operations.
//!
//! Library code reports progress with [`status`]; it is a no-op unless a
//! [`Spinner`] is running, and the spinner only draws when stderr is a TTY
//! (never in the daemon's logs, pipes or tests).

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

static MESSAGE: Mutex<Option<String>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Update the message of the running spinner (if any).
pub fn status(msg: impl Into<String>) {
    if ACTIVE.load(Ordering::Relaxed)
        && let Ok(mut m) = MESSAGE.lock()
    {
        *m = Some(msg.into());
    }
}

pub struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    started: Instant,
}

impl Spinner {
    pub fn start(msg: &str) -> Spinner {
        let stop = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        if !std::io::stderr().is_terminal() || std::env::var_os("SB_NO_SPINNER").is_some() {
            return Spinner { stop, handle: None, started };
        }
        if let Ok(mut m) = MESSAGE.lock() {
            *m = Some(msg.to_string());
        }
        ACTIVE.store(true, Ordering::Relaxed);
        let s = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut i = 0;
            let mut err = std::io::stderr();
            let _ = write!(err, "\x1b[?25l"); // hide cursor
            while !s.load(Ordering::Relaxed) {
                let msg = MESSAGE.lock().ok().and_then(|m| m.clone()).unwrap_or_default();
                let secs = started.elapsed().as_secs_f32();
                let _ =
                    write!(err, "\r\x1b[2K\x1b[36m{}\x1b[0m {msg} \x1b[2m{secs:.1}s\x1b[0m", FRAMES[i % FRAMES.len()]);
                let _ = err.flush();
                i += 1;
                std::thread::sleep(Duration::from_millis(80));
            }
            let _ = write!(err, "\r\x1b[2K\x1b[?25h"); // clear line, show cursor
            let _ = err.flush();
        });
        Spinner { stop, handle: Some(handle), started }
    }

    /// Stop and print a final ✔ line with the elapsed time.
    pub fn done(mut self, msg: &str) {
        let animated = self.handle.is_some();
        self.finish();
        if animated {
            eprintln!("\x1b[32m✔\x1b[0m {msg} \x1b[2m({:.1}s)\x1b[0m", self.started.elapsed().as_secs_f32());
        }
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        ACTIVE.store(false, Ordering::Relaxed);
    }
}

impl Drop for Spinner {
    /// Errors/early returns still restore the terminal.
    fn drop(&mut self) {
        self.finish();
    }
}
