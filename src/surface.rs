//! Shared-memory surfaces.
//!
//! On Windows a surface is a file mapping Casement created and duplicated into
//! this process; `handle` is already valid here and is mapped with
//! `MapViewOfFile`. Elsewhere there are no such handles, so for tests the
//! harness creates a plain file `surface-<handle>` in the directory named by
//! `CASEMENT_SURFACE_DIR` and the plugin maps that instead.

use std::io;

/// Environment variable naming the directory of emulated surfaces (not Windows).
pub const SURFACE_DIR_ENV: &str = "CASEMENT_SURFACE_DIR";

/// A mapped surface: 32-bit BGRX, top row first, `stride` bytes per row.
pub struct Surface {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    map: Mapping,
}

impl Surface {
    /// Maps the surface Casement described in a `surface` message.
    pub fn open(
        id: u32,
        handle: u64,
        width: u32,
        height: u32,
        stride: u32,
        size: u64,
    ) -> io::Result<Self> {
        let need = stride as u64 * height as u64;
        if (stride as u64) < width as u64 * 4 || size < need || size > usize::MAX as u64 {
            Mapping::discard(handle);
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("surface {id}: {width}x{height}, stride {stride}, size {size} don't fit together"),
            ));
        }
        let map = Mapping::open(handle, size as usize)?;
        Ok(Surface {
            id,
            width,
            height,
            stride,
            map,
        })
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        self.map.bytes_mut()
    }
}

#[cfg(windows)]
struct Mapping {
    handle: windows_sys::Win32::Foundation::HANDLE,
    view: windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
}

// The view is plain memory owned by this struct.
#[cfg(windows)]
unsafe impl Send for Mapping {}

#[cfg(windows)]
impl Mapping {
    fn open(handle: u64, len: usize) -> io::Result<Self> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Memory::{MapViewOfFile, FILE_MAP_ALL_ACCESS};
        let handle = handle as usize as windows_sys::Win32::Foundation::HANDLE;
        // SAFETY: Casement duplicated `handle` into this process for us; a bad
        // value makes MapViewOfFile fail rather than touch anything else.
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, len) };
        if view.Value.is_null() {
            let err = io::Error::last_os_error();
            unsafe { CloseHandle(handle) };
            return Err(err);
        }
        Ok(Mapping { handle, view, len })
    }

    fn discard(handle: u64) {
        let handle = handle as usize as windows_sys::Win32::Foundation::HANDLE;
        // SAFETY: the handle is ours to close (see `open`).
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: the view is `len` bytes long and lives as long as `self`.
        unsafe { std::slice::from_raw_parts_mut(self.view.Value as *mut u8, self.len) }
    }
}

#[cfg(windows)]
impl Drop for Mapping {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Memory::UnmapViewOfFile;
        // SAFETY: both were obtained in `open` and are released exactly once.
        unsafe {
            UnmapViewOfFile(self.view);
            CloseHandle(self.handle);
        }
    }
}

#[cfg(not(windows))]
struct Mapping {
    map: memmap2::MmapMut,
}

#[cfg(not(windows))]
impl Mapping {
    fn open(handle: u64, len: usize) -> io::Result<Self> {
        let dir = std::env::var_os(SURFACE_DIR_ENV).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!("surfaces need {SURFACE_DIR_ENV} outside Windows"),
            )
        })?;
        let path = std::path::Path::new(&dir).join(format!("surface-{handle}"));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)?;
        if file.metadata()?.len() < len as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is shorter than {len} bytes", path.display()),
            ));
        }
        // SAFETY: the test harness owns the file and only reads it while no
        // frame is in flight, mirroring the protocol's rules.
        let map = unsafe { memmap2::MmapOptions::new().len(len).map_mut(&file)? };
        Ok(Mapping { map })
    }

    fn discard(_handle: u64) {}

    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.map
    }
}
