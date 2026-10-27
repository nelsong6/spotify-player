//! The sound card side of the home cast. It plays on the system's default output and reopens on
//! the default device when its own goes away (unplugged, dropped off USB); on Windows it also moves
//! to the new default when the system's default output changes. The sound card's callback is told
//! when its buffer reaches the speaker, which is what keeps the PCs in step.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use super::clock::Clock;
use super::protocol::{Format, Micros};
use super::resample::Resampler;
use super::stream::Stream;
use super::Mute;

/// How long to wait between attempts to open an output while there is none
const REOPEN_INTERVAL: Duration = Duration::from_secs(2);
/// The stream is always stereo; the session's player makes nothing else
pub const CHANNELS: usize = 2;

/// What the network thread and the sound card's callback share
pub struct Shared {
    pub stream: Stream,
    input: Option<Format>,
    out_rate: Option<u32>,
    resampler: Option<Resampler>,
    buffer: Micros,
    /// Reused between chunks to avoid an allocation per chunk
    decoded: Vec<f32>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            stream: Stream::new(48_000, CHANNELS),
            input: None,
            out_rate: None,
            resampler: None,
            buffer: 1_000_000,
            decoded: Vec::new(),
        }
    }
}

impl Shared {
    fn reset(&mut self) {
        self.resampler = match (self.input, self.out_rate) {
            (Some(input), Some(out)) => Some(Resampler::new(CHANNELS, input.rate, out)),
            _ => None,
        };
        self.stream = Stream::new(self.out_rate.unwrap_or(48_000), CHANNELS);
        self.stream.set_buffer(self.buffer);
    }

    pub fn set_input(&mut self, format: Format) {
        self.input = Some(format);
        self.reset();
    }

    pub fn set_buffer(&mut self, buffer: Micros) {
        self.buffer = buffer;
        self.stream.set_buffer(buffer);
    }

    fn set_output_rate(&mut self, rate: u32) {
        if self.out_rate != Some(rate) {
            self.out_rate = Some(rate);
            self.reset();
        }
    }

    /// Queue a chunk of 16-bit PCM recorded at server time `start`
    pub fn push_chunk(&mut self, start: Micros, pcm: &[u8], server_now: Micros) {
        let (Some(input), Some(resampler)) = (self.input, self.resampler.as_mut()) else {
            return;
        };
        self.decoded.clear();
        self.decoded.extend(
            pcm.as_chunks::<2>()
                .0
                .iter()
                .map(|b| f32::from(i16::from_le_bytes(*b)) / 32_768.0),
        );
        let offset = resampler.next_offset();
        let mut samples = Vec::with_capacity(self.decoded.len() * 12 / 10 + 8);
        resampler.process(&self.decoded, &mut samples);
        // the first output frame sits a fraction of a frame before or after the chunk's first
        let start = start + (offset * 1e6 / f64::from(input.rate)) as Micros;
        self.stream.push(start, samples, server_now);
    }
}

enum Event {
    Lost,
    #[cfg(windows)]
    DefaultChanged,
}

/// Play the stream on the default output for as long as the process runs
pub fn run(
    shared: &Arc<Mutex<Shared>>,
    clock: &Arc<Clock>,
    mute: &Arc<Mute>,
    played: Option<&flume::Sender<(u32, Vec<f32>)>>,
) {
    let (events, changes) = flume::unbounded();
    #[cfg(windows)]
    let _watch = default_device::Watch::register(events.clone())
        .inspect_err(|err| {
            tracing::warn!("Failed to watch for default audio device changes: {err:#}");
        })
        .ok();
    loop {
        match open(shared, clock, mute, played.cloned(), events.clone()) {
            Ok(stream) => {
                match changes.recv() {
                    Ok(Event::Lost) => {
                        tracing::warn!(
                            "The audio output device went away; reopening on the default device"
                        );
                    }
                    #[cfg(windows)]
                    Ok(Event::DefaultChanged) => {
                        tracing::info!("The default audio output device changed; reopening on it");
                    }
                    Err(_) => return,
                }
                drop(stream);
                while changes.try_recv().is_ok() {}
            }
            Err(err) => {
                tracing::warn!("Failed to open an audio output: {err:#}");
                let _ = changes.recv_timeout(REOPEN_INTERVAL);
            }
        }
    }
}

fn open(
    shared: &Arc<Mutex<Shared>>,
    clock: &Arc<Clock>,
    mute: &Arc<Mute>,
    played: Option<flume::Sender<(u32, Vec<f32>)>>,
    events: flume::Sender<Event>,
) -> anyhow::Result<cpal::Stream> {
    let device = cpal::default_host()
        .default_output_device()
        .context("no audio output device")?;
    let config = device
        .default_output_config()
        .context("read the device's format")?;
    tracing::info!(
        "Using audio device: {} ({config:?})",
        device
            .name()
            .unwrap_or_else(|_| "[unknown name]".to_string())
    );
    let rate = config.sample_rate().0;
    shared.lock().set_output_rate(rate);
    let callback = Callback {
        shared: Arc::clone(shared),
        clock: Arc::clone(clock),
        mute: Arc::clone(mute),
        played,
        rate,
        device_channels: usize::from(config.channels()),
        buf: Vec::new(),
    };
    let on_error = move |err| {
        tracing::warn!("Audio stream error: {err}");
        if matches!(err, cpal::StreamError::DeviceNotAvailable) {
            let _ = events.send(Event::Lost);
        }
    };
    let stream_config = config.config();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, &stream_config, callback, on_error),
        cpal::SampleFormat::I16 => build::<i16>(&device, &stream_config, callback, on_error),
        cpal::SampleFormat::I32 => build::<i32>(&device, &stream_config, callback, on_error),
        cpal::SampleFormat::U16 => build::<u16>(&device, &stream_config, callback, on_error),
        other => anyhow::bail!("unsupported sample format {other}"),
    }?;
    stream.play()?;
    Ok(stream)
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut callback: Callback,
    on_error: impl FnMut(cpal::StreamError) + Send + 'static,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| callback.fill(data, info),
        on_error,
        None,
    )?)
}

struct Callback {
    shared: Arc<Mutex<Shared>>,
    clock: Arc<Clock>,
    mute: Arc<Mute>,
    /// Receives what plays, for the visualization
    played: Option<flume::Sender<(u32, Vec<f32>)>>,
    rate: u32,
    device_channels: usize,
    buf: Vec<f32>,
}

impl Callback {
    fn fill<T: cpal::Sample + cpal::FromSample<f32>>(
        &mut self,
        data: &mut [T],
        info: &cpal::OutputCallbackInfo,
    ) {
        let ts = info.timestamp();
        let dac = ts
            .playback
            .duration_since(&ts.callback)
            .map_or(0, |d| d.as_micros() as Micros);
        let frames = data.len() / self.device_channels;
        self.buf.resize(frames * CHANNELS, 0.0);
        let is_audio = self.clock.is_synced()
            && self
                .shared
                .lock()
                .stream
                .fill(&mut self.buf, self.clock.server_now(), dac);
        if !is_audio {
            self.buf.fill(0.0);
        } else if let Some(played) = &self.played {
            let _ = played.try_send((self.rate, self.buf.clone()));
        }
        let gain = if self.mute.silent() { 0.0 } else { 1.0 };
        for (out, src) in data
            .chunks_mut(self.device_channels)
            .zip(self.buf.chunks(CHANNELS))
        {
            if let [only] = out {
                *only = T::from_sample(f32::midpoint(src[0], src[1]) * gain);
                continue;
            }
            for (i, sample) in out.iter_mut().enumerate() {
                let v = src.get(i).copied().unwrap_or(0.0);
                *sample = T::from_sample(v * gain);
            }
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)] // used to interact with the Windows API
mod default_device {
    use windows::core::{implement, Result, PCWSTR};
    use windows::Win32::Media::Audio::{
        eConsole, eRender, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
        IMMNotificationClient_Impl, MMDeviceEnumerator, DEVICE_STATE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;

    use super::Event;

    /// A registration for Windows' notice that the default output device changed. It stays
    /// registered until dropped.
    pub struct Watch {
        enumerator: IMMDeviceEnumerator,
        client: IMMNotificationClient,
    }

    impl Watch {
        pub fn register(events: flume::Sender<Event>) -> anyhow::Result<Self> {
            // cpal initializes COM the same way on this thread; an already initialized thread
            // returns S_FALSE, and the matching uninitialize is left to thread exit.
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok()?;
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
            let client: IMMNotificationClient = Client { events }.into();
            unsafe { enumerator.RegisterEndpointNotificationCallback(&client) }?;
            Ok(Self { enumerator, client })
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            if let Err(err) = unsafe {
                self.enumerator
                    .UnregisterEndpointNotificationCallback(&self.client)
            } {
                tracing::warn!("Failed to stop watching the default audio device: {err:#}");
            }
        }
    }

    /// Called by Windows on its own thread, so it only passes the notice on
    #[implement(IMMNotificationClient)]
    struct Client {
        events: flume::Sender<Event>,
    }

    impl IMMNotificationClient_Impl for Client_Impl {
        fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _: &PCWSTR) -> Result<()> {
            // cpal's default output is the console role's render device
            if flow == eRender && role == eConsole {
                let _ = self.events.send(Event::DefaultChanged);
            }
            Ok(())
        }

        fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> Result<()> {
            Ok(())
        }

        fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> {
            Ok(())
        }

        fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> {
            Ok(())
        }

        fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> Result<()> {
            Ok(())
        }
    }
}
