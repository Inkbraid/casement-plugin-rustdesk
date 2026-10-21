//! What a connection (the test pattern, a RustDesk peer) looks like to the
//! main loop.

use crate::protocol::FromCasement;

pub trait Session: Send {
    /// Input and clipboard from Casement: `key`, `pointer`, `wheel`,
    /// `clipboard`, `resize`, `mute` and anything newer. Must not block.
    fn input(&mut self, msg: &FromCasement);

    /// Casement sent `disconnect` or closed stdin: close the connection.
    /// Casement ends the process two seconds later, so keep this short.
    fn disconnect(&mut self);
}
