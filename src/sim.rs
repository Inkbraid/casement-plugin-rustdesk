//! Casement's side of the protocol, for tests and `examples/casement-sim.rs`:
//! starts the plugin, exchanges messages with it and provides surfaces the
//! way Casement does (a duplicated file mapping on Windows, a shared file in
//! `CASEMENT_SURFACE_DIR` elsewhere).

use crate::protocol::{read_message, write_message, FromCasement, ToCasement};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Casement {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Receiver<ToCasement>,
    stderr: Arc<Mutex<Vec<String>>>,
    #[cfg_attr(windows, allow(dead_code))]
    dir: PathBuf,
    #[cfg_attr(windows, allow(dead_code))]
    next_handle: u64,
}

impl Casement {
    /// Starts `program` with piped stdio. With `echo_stderr` the plugin's log
    /// is also copied to this process's stderr.
    pub fn spawn(program: &Path, echo_stderr: bool) -> io::Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "casement-sim-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        let mut child = Command::new(program)
            .env(crate::surface::SURFACE_DIR_ENV, &dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("piped stdout");
        let stderr_pipe = child.stderr.take().expect("piped stderr");

        let (tx, messages) = mpsc::channel();
        std::thread::spawn(move || loop {
            match read_message::<_, ToCasement>(&mut stdout) {
                Ok(Some(m)) => {
                    if tx.send(m).is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    eprintln!("sim: bad message from the plugin: {e}");
                    return;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let lines = stderr.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr_pipe).lines() {
                let Ok(line) = line else { return };
                if echo_stderr {
                    eprintln!("plugin: {line}");
                }
                lines.lock().unwrap().push(line);
            }
        });
        Ok(Casement {
            child,
            stdin,
            messages,
            stderr,
            dir,
            next_handle: 100,
        })
    }

    pub fn send(&mut self, msg: &FromCasement) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "stdin closed"))?;
        write_message(stdin, msg)
    }

    /// Sends a raw frame body (for malformed-message tests).
    pub fn send_raw(&mut self, body: &[u8]) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "stdin closed"))?;
        crate::protocol::write_frame(stdin, body)
    }

    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// The next message, or `None` after `timeout` or when the plugin's
    /// stdout closed.
    pub fn recv(&self, timeout: Duration) -> Option<ToCasement> {
        self.messages.recv_timeout(timeout).ok()
    }

    /// Waits for a message `pick` accepts, dropping the others.
    pub fn expect<T>(
        &self,
        timeout: Duration,
        mut pick: impl FnMut(&ToCasement) -> Option<T>,
    ) -> Option<T> {
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let m = self.recv(left)?;
            if let Some(t) = pick(&m) {
                return Some(t);
            }
        }
    }

    pub fn stderr(&self) -> Vec<String> {
        self.stderr.lock().unwrap().clone()
    }

    pub fn wait(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let end = Instant::now() + timeout;
        loop {
            if let Ok(Some(s)) = self.child.try_wait() {
                return Some(s);
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Creates a surface for the plugin and the `surface` message to send.
    /// Rows get `pad` extra bytes, to exercise strides.
    pub fn create_surface(
        &mut self,
        id: u32,
        width: u32,
        height: u32,
        pad: u32,
    ) -> io::Result<(HostSurface, FromCasement)> {
        let stride = width * 4 + pad;
        let size = stride as u64 * height as u64;
        let (surface, handle) = HostSurface::create(self, size as usize)?;
        Ok((
            surface,
            FromCasement::Surface {
                id,
                handle,
                width,
                height,
                stride,
                size,
            },
        ))
    }
}

static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl Drop for Casement {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Casement's view of a surface.
pub struct HostSurface {
    imp: imp::Mapping,
}

impl HostSurface {
    fn create(c: &mut Casement, size: usize) -> io::Result<(Self, u64)> {
        let (imp, handle) = imp::Mapping::create(c, size)?;
        Ok((HostSurface { imp }, handle))
    }

    pub fn pixels(&self) -> &[u8] {
        self.imp.bytes()
    }
}

#[cfg(windows)]
mod imp {
    use super::Casement;
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{
        CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
        MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub struct Mapping {
        handle: HANDLE,
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        len: usize,
    }

    impl Mapping {
        pub fn create(c: &mut Casement, len: usize) -> io::Result<(Self, u64)> {
            // SAFETY: standard Win32 calls on handles this function owns.
            unsafe {
                let handle = CreateFileMappingW(
                    INVALID_HANDLE_VALUE,
                    std::ptr::null(),
                    PAGE_READWRITE,
                    (len as u64 >> 32) as u32,
                    len as u32,
                    std::ptr::null(),
                );
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, len);
                if view.Value.is_null() {
                    let e = io::Error::last_os_error();
                    CloseHandle(handle);
                    return Err(e);
                }
                let mut target: HANDLE = std::ptr::null_mut();
                let ok = DuplicateHandle(
                    GetCurrentProcess(),
                    handle,
                    c.child.as_raw_handle() as HANDLE,
                    &mut target,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                );
                let m = Mapping { handle, view, len };
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok((m, target as usize as u64))
            }
        }

        pub fn bytes(&self) -> &[u8] {
            // SAFETY: the view is `len` bytes and lives as long as `self`.
            unsafe { std::slice::from_raw_parts(self.view.Value as *const u8, self.len) }
        }
    }

    impl Drop for Mapping {
        fn drop(&mut self) {
            // SAFETY: released once, obtained in `create`.
            unsafe {
                UnmapViewOfFile(self.view);
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Casement;
    use std::io;

    pub struct Mapping {
        map: memmap2::MmapMut,
    }

    impl Mapping {
        pub fn create(c: &mut Casement, len: usize) -> io::Result<(Self, u64)> {
            let handle = c.next_handle;
            c.next_handle += 4;
            let path = c.dir.join(format!("surface-{handle}"));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)?;
            file.set_len(len as u64)?;
            // SAFETY: the file is private to this test run.
            let map = unsafe { memmap2::MmapOptions::new().len(len).map_mut(&file)? };
            Ok((Mapping { map }, handle))
        }

        pub fn bytes(&self) -> &[u8] {
            &self.map
        }
    }
}
