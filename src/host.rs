//! The plugin's side of the connection to Casement: the protocol stream on
//! stdout and the display that feeds surfaces.

use crate::display::Display;
use crate::protocol::{write_message, State, ToCasement};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

/// The protocol output stream. Writes are serialised so messages from
/// different threads never interleave.
pub struct Output {
    out: Mutex<Box<dyn Write + Send>>,
}

impl Output {
    pub fn new(out: Box<dyn Write + Send>) -> Self {
        Output {
            out: Mutex::new(out),
        }
    }

    /// Sends one message. If Casement has gone away there is nobody left to
    /// talk to, so a broken pipe ends the process.
    pub fn send(&self, msg: &ToCasement) {
        let result = {
            let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
            write_message(&mut *out, msg)
        };
        if let Err(e) = result {
            log::error!("can't write to Casement: {e}");
            std::process::exit(3);
        }
    }

    pub fn try_send(&self, msg: &ToCasement) -> io::Result<()> {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        write_message(&mut *out, msg)
    }
}

/// Everything a session needs to talk to Casement.
pub struct Host {
    pub out: Arc<Output>,
    pub display: Mutex<Display>,
}

impl Host {
    pub fn new(out: Box<dyn Write + Send>) -> Arc<Self> {
        let out = Arc::new(Output::new(out));
        Arc::new(Host {
            display: Mutex::new(Display::new(out.clone())),
            out,
        })
    }

    pub fn send(&self, msg: &ToCasement) {
        self.out.send(msg)
    }

    pub fn display(&self) -> std::sync::MutexGuard<'_, Display> {
        self.display.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reports a final state and ends the process: `closed` and `auth_failed`
    /// exit with 0, `failed` with 1.
    pub fn finish(&self, state: State, text: &str) -> ! {
        match state {
            State::Failed => log::error!("failed: {text}"),
            _ => log::info!("{state:?} {text}"),
        }
        let _ = self.out.try_send(&ToCasement::state(state, text));
        std::process::exit(if state == State::Failed { 1 } else { 0 })
    }
}
