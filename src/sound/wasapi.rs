//! The sound device on Windows: an event-driven WASAPI stream in shared
//! mode on the default output device. It asks for the stream's own format
//! and lets Windows convert; a device that won't gets its mix format,
//! converted here ([`super::Convert`]). The render thread follows the
//! default device when the user changes it, through [`Device::current`].

use super::{Convert, Device, DeviceFormat, Format, Sample, DEVICE_BUFFER};
use std::time::Duration;
use windows::core::{Error, HRESULT};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_DEVICE_IN_USE,
    AUDCLNT_E_ENDPOINT_CREATE_FAILED, AUDCLNT_E_SERVICE_NOT_RUNNING, AUDCLNT_E_UNSUPPORTED_FORMAT,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0, WAVE_FORMAT_PCM,
};
use windows::Win32::Media::KernelStreaming::{
    KSDATAFORMAT_SUBTYPE_PCM, SPEAKER_FRONT_CENTER, SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT,
    WAVE_FORMAT_EXTENSIBLE,
};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// COM on the render thread, for as long as it runs.
pub struct Com(bool);

impl Com {
    pub fn init() -> Self {
        // SAFETY: sets up COM for this thread; undone in drop on the same
        // thread (Com isn't Send).
        Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx in `init`.
            unsafe { CoUninitialize() }
        }
    }
}

pub struct Wasapi {
    enumerator: IMMDeviceEnumerator,
    /// The endpoint ID, to notice when the default output changes.
    id: String,
    client: IAudioClient,
    render: IAudioRenderClient,
    /// Signalled by Windows whenever the device wants more sound.
    event: HANDLE,
    buffer_frames: u32,
    stream: Format,
    device_rate: u32,
    frame_bytes: usize,
    /// Set when the device takes only its own format.
    convert: Option<Convert>,
    converted: Vec<u8>,
}

impl Wasapi {
    pub fn open(format: Format) -> Result<Self, String> {
        // SAFETY: COM calls on interfaces this function holds, on a thread
        // set up for COM (see `Com`); the format structures outlive the
        // calls that read them, the mix format is freed once read, and the
        // event is closed by the device (or here, if it isn't made).
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| text("no audio devices", e))?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|e| text("no default audio output", e))?;
            let id = device_id(&device).map_err(|e| text("no default audio output", e))?;
            let activate = || -> Result<IAudioClient, String> {
                device
                    .Activate(CLSCTX_ALL, None)
                    .map_err(|e| text("can't open the audio output", e))
            };

            let client = activate()?;
            let want = float_format(format);
            let wanted = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                hns(DEVICE_BUFFER),
                0,
                std::ptr::addr_of!(want).cast(),
                None,
            );
            let (client, device_format, convert) = match wanted {
                Ok(()) => {
                    let df = DeviceFormat {
                        rate: format.rate,
                        channels: format.channels,
                        sample: Sample::F32,
                    };
                    (client, df, None)
                }
                Err(_) => {
                    // Then the device's own format, converted here.
                    let client = activate()?;
                    let mix = client
                        .GetMixFormat()
                        .map_err(|e| text("can't start the audio output", e))?;
                    let df = device_format(mix);
                    let initialized = match df {
                        Some(_) => client.Initialize(
                            AUDCLNT_SHAREMODE_SHARED,
                            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                            hns(DEVICE_BUFFER),
                            0,
                            mix,
                            None,
                        ),
                        None => Err(AUDCLNT_E_UNSUPPORTED_FORMAT.into()),
                    };
                    CoTaskMemFree(Some(mix.cast_const().cast()));
                    initialized.map_err(|e| text("can't start the audio output", e))?;
                    let df = df.expect("checked above");
                    (client, df, Some(Convert::new(format, df)))
                }
            };
            let start = |e| text("can't start the audio output", e);
            let buffer_frames = client.GetBufferSize().map_err(start)?;
            let render: IAudioRenderClient = client.GetService().map_err(start)?;
            let event = CreateEventW(None, false, false, None).map_err(start)?;
            if let Err(e) = client.SetEventHandle(event) {
                let _ = CloseHandle(event);
                return Err(start(e));
            }
            log::info!(
                "sound: {} Hz, {} channels on the default output{}",
                device_format.rate,
                device_format.channels,
                if convert.is_some() {
                    " (converted here)"
                } else {
                    ""
                }
            );
            Ok(Wasapi {
                enumerator,
                id,
                client,
                render,
                event,
                buffer_frames,
                stream: format,
                device_rate: device_format.rate,
                frame_bytes: usize::from(device_format.channels) * device_format.sample.bytes(),
                convert,
                converted: Vec::new(),
            })
        }
    }

    /// Device frames free in the buffer.
    fn free(&mut self) -> Result<u32, String> {
        // SAFETY: a COM call on an interface this device holds.
        let padding = unsafe { self.client.GetCurrentPadding() }.map_err(|e| text("", e))?;
        Ok(self.buffer_frames.saturating_sub(padding))
    }
}

impl Device for Wasapi {
    fn wait(&mut self, timeout: Duration) {
        // SAFETY: the event belongs to this device and is open.
        unsafe { WaitForSingleObject(self.event, timeout.as_millis() as u32) };
    }

    fn room(&mut self) -> Result<usize, String> {
        let free = u64::from(self.free()?);
        if self.convert.is_none() {
            return Ok(free as usize);
        }
        // In the stream's frames, a frame short: resampling may make one
        // more.
        Ok(
            (free.saturating_sub(1) * u64::from(self.stream.rate) / u64::from(self.device_rate))
                as usize,
        )
    }

    fn write(&mut self, samples: &[f32]) -> Result<(), String> {
        let free = self.free()?;
        let (bytes, frames) = match self.convert.as_mut() {
            Some(c) => {
                let frames = c.run(samples, &mut self.converted);
                (self.converted.as_slice(), frames)
            }
            None => {
                // SAFETY: f32 has no padding or invalid bit patterns; the
                // bytes view covers exactly the slice.
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        samples.as_ptr().cast::<u8>(),
                        std::mem::size_of_val(samples),
                    )
                };
                (bytes, samples.len() / usize::from(self.stream.channels))
            }
        };
        let count = (frames as u32).min(free);
        if count == 0 {
            return Ok(());
        }
        // SAFETY: GetBuffer hands out room for `count` frames of
        // `frame_bytes`, which is filled from `bytes` (at least that long)
        // and released at once.
        unsafe {
            let out = self.render.GetBuffer(count).map_err(|e| text("", e))?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, count as usize * self.frame_bytes);
            self.render
                .ReleaseBuffer(count, 0)
                .map_err(|e| text("", e))?;
        }
        Ok(())
    }

    fn start(&mut self) -> Result<(), String> {
        // SAFETY: a COM call on an interface this device holds.
        unsafe { self.client.Start() }.map_err(|e| text("", e))
    }

    fn stop(&mut self) {
        // SAFETY: COM calls on an interface this device holds; Reset needs
        // the stream stopped, which it is by then.
        unsafe {
            let _ = self.client.Stop();
            let _ = self.client.Reset();
        }
    }

    fn current(&mut self) -> bool {
        // SAFETY: COM calls on an interface this device holds and on the
        // device it returns.
        unsafe {
            self.enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .and_then(|d| device_id(&d))
        }
        .is_ok_and(|id| id == self.id)
    }
}

impl Drop for Wasapi {
    fn drop(&mut self) {
        // SAFETY: a COM call on an interface this device holds, and the
        // device's own event, closed once.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// 32-bit float in the stream's rate and channels.
fn float_format(f: Format) -> WAVEFORMATEXTENSIBLE {
    let block = f.channels * 4;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: f.channels,
            nSamplesPerSec: f.rate,
            nAvgBytesPerSec: f.rate * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: 32,
            cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>()
                - std::mem::size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: 32,
        },
        dwChannelMask: match f.channels {
            1 => SPEAKER_FRONT_CENTER,
            2 => SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
            _ => 0,
        },
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    }
}

/// What a mix format is, if it's one [`Convert`] can write.
///
/// # Safety
/// `p` points to a valid `WAVEFORMATEX`, followed by the rest of a
/// `WAVEFORMATEXTENSIBLE` when its tag says so.
unsafe fn device_format(p: *const WAVEFORMATEX) -> Option<DeviceFormat> {
    let wf = p.read_unaligned();
    let tag = u32::from(wf.wFormatTag);
    let sub = (tag == WAVE_FORMAT_EXTENSIBLE && wf.cbSize >= 22).then(|| {
        let ext = p.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned();
        ext.SubFormat
    });
    let float = tag == WAVE_FORMAT_IEEE_FLOAT || sub == Some(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
    let pcm = tag == WAVE_FORMAT_PCM || sub == Some(KSDATAFORMAT_SUBTYPE_PCM);
    let sample = match wf.wBitsPerSample {
        32 if float => Sample::F32,
        16 if pcm => Sample::I16,
        32 if pcm => Sample::I32,
        _ => return None,
    };
    Some(DeviceFormat {
        rate: wf.nSamplesPerSec,
        channels: wf.nChannels,
        sample,
    })
}

/// The endpoint ID of a device.
///
/// # Safety
/// A COM call; see [`Wasapi::open`].
unsafe fn device_id(device: &IMMDevice) -> windows::core::Result<String> {
    let p = device.GetId()?;
    let id = p.to_string();
    CoTaskMemFree(Some(p.0.cast_const().cast()));
    id.map_err(|_| Error::from(AUDCLNT_E_DEVICE_INVALIDATED))
}

/// A duration in WASAPI's 100-nanosecond units.
fn hns(d: Duration) -> i64 {
    (d.as_nanos() / 100) as i64
}

/// An error for the log: what failed and why, with WASAPI's codes (which
/// Windows has no text for) spelled out.
fn text(what: &str, e: Error) -> String {
    let code: HRESULT = e.code();
    let known = [
        (
            AUDCLNT_E_DEVICE_INVALIDATED,
            "the device was removed or changed",
        ),
        (
            AUDCLNT_E_SERVICE_NOT_RUNNING,
            "the Windows Audio service isn't running",
        ),
        (
            AUDCLNT_E_UNSUPPORTED_FORMAT,
            "the device takes no format this plugin can write",
        ),
        (
            AUDCLNT_E_DEVICE_IN_USE,
            "another program has the device to itself",
        ),
        (
            AUDCLNT_E_ENDPOINT_CREATE_FAILED,
            "the device couldn't be opened",
        ),
    ];
    let why = known
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, t)| t.to_string())
        .unwrap_or_else(|| e.message().trim().to_owned());
    let why = if why.is_empty() {
        "error".to_owned()
    } else {
        why
    };
    let code = code.0 as u32;
    if what.is_empty() {
        format!("{why} (0x{code:08X})")
    } else {
        format!("{what}: {why} (0x{code:08X})")
    }
}
