//! Keeping stdout for protocol messages only.
//!
//! The protocol stream is taken over as a private handle, and the process's
//! standard output is pointed at stderr, so a stray `println!` (ours or a
//! library's) ends up in Casement's log instead of corrupting the stream.

use std::fs::File;
use std::io::{self, Write};

/// Takes the protocol output stream and redirects standard output to stderr.
pub fn take_protocol_output() -> Box<dyn Write + Send> {
    match take() {
        Ok(f) => Box::new(f),
        Err(e) => {
            log::warn!("keeping stdout as it is: {e}");
            Box::new(io::stdout())
        }
    }
}

#[cfg(unix)]
fn take() -> io::Result<File> {
    use std::os::fd::FromRawFd;
    // SAFETY: plain descriptor juggling on descriptors this process owns.
    unsafe {
        let fd = libc::dup(1);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        if libc::dup2(2, 1) < 0 {
            let e = io::Error::last_os_error();
            libc::close(fd);
            return Err(e);
        }
        Ok(File::from_raw_fd(fd))
    }
}

#[cfg(windows)]
fn take() -> io::Result<File> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };
    // SAFETY: the handles are the process's own standard handles; stdout's
    // ends up owned by the returned File and nothing else uses it afterwards.
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        if out.is_null() || out == INVALID_HANDLE_VALUE {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no stdout"));
        }
        let err = GetStdHandle(STD_ERROR_HANDLE);
        if SetStdHandle(STD_OUTPUT_HANDLE, err) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(File::from_raw_handle(out as _))
    }
}

#[cfg(not(any(unix, windows)))]
fn take() -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "unsupported platform",
    ))
}

/// Sets up logging to stderr. `CASEMENT_RUSTDESK_LOG` takes an env_logger
/// filter (default `info`).
pub fn init_logging() {
    use std::io::Write as _;
    let env = env_logger::Env::new().filter_or("CASEMENT_RUSTDESK_LOG", "info");
    env_logger::Builder::from_env(env)
        .target(env_logger::Target::Stderr)
        .format(|buf, r| writeln!(buf, "{} {}: {}", r.level(), r.target(), r.args()))
        .init();
}
