//! The sound card side of the home cast. It plays on the system's default output and on any other
//! outputs chosen on this PC, each in step with the stream. It reopens on the default device when
//! its own goes away (unplugged, dropped off USB); on Windows it also moves to the new default when
//! the system's default output changes, and picks up a chosen output when it appears (a TV switched
//! on). The sound card's callback is told when its buffer reaches the speaker, which is what keeps
//! the PCs, and a PC's outputs, in step.

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

/// How long to wait between attempts to open an output that failed to open
const REOPEN_INTERVAL: Duration = Duration::from_secs(2);
/// The stream is always stereo; the session's player makes nothing else
pub const CHANNELS: usize = 2;

/// The stream as the network thread receives it, passed on to every output playing it
pub struct Feed {
    input: Option<Format>,
    buffer: Micros,
    outputs: Vec<Arc<Mutex<Shared>>>,
}

impl Default for Feed {
    fn default() -> Self {
        Self {
            input: None,
            buffer: 1_000_000,
            outputs: Vec::new(),
        }
    }
}

impl Feed {
    pub fn set_input(&mut self, format: Format) {
        self.input = Some(format);
        for output in &self.outputs {
            output.lock().set_input(format);
        }
    }

    pub fn set_buffer(&mut self, buffer: Micros) {
        self.buffer = buffer;
        for output in &self.outputs {
            output.lock().set_buffer(buffer);
        }
    }

    pub fn clear(&mut self) {
        for output in &self.outputs {
            output.lock().stream.clear();
        }
    }

    /// Queue a chunk of 16-bit PCM recorded at server time `start`
    pub fn push_chunk(&mut self, start: Micros, pcm: &[u8], server_now: Micros) {
        for output in &self.outputs {
            output.lock().push_chunk(start, pcm, server_now);
        }
    }

    /// A new output's copy of the stream, fed once attached
    fn output(&self, out_rate: u32) -> Arc<Mutex<Shared>> {
        let mut shared = Shared {
            stream: Stream::new(out_rate, CHANNELS),
            input: self.input,
            out_rate,
            resampler: None,
            buffer: self.buffer,
            decoded: Vec::new(),
        };
        shared.reset();
        Arc::new(Mutex::new(shared))
    }

    fn attach(&mut self, shared: &Arc<Mutex<Shared>>) {
        // the format or buffer may have changed while the output's stream was opening
        let mut output = shared.lock();
        if output.input != self.input || output.buffer != self.buffer {
            output.input = self.input;
            output.buffer = self.buffer;
            output.reset();
        }
        drop(output);
        self.outputs.push(Arc::clone(shared));
    }

    fn detach(&mut self, shared: &Arc<Mutex<Shared>>) {
        self.outputs.retain(|output| !Arc::ptr_eq(output, shared));
    }
}

/// What the network thread and one output's sound card callback share
struct Shared {
    stream: Stream,
    input: Option<Format>,
    out_rate: u32,
    resampler: Option<Resampler>,
    buffer: Micros,
    /// Reused between chunks to avoid an allocation per chunk
    decoded: Vec<f32>,
}

impl Shared {
    fn reset(&mut self) {
        self.resampler = self
            .input
            .map(|input| Resampler::new(CHANNELS, input.rate, self.out_rate));
        self.stream = Stream::new(self.out_rate, CHANNELS);
        self.stream.set_buffer(self.buffer);
    }

    fn set_input(&mut self, format: Format) {
        self.input = Some(format);
        self.reset();
    }

    fn set_buffer(&mut self, buffer: Micros) {
        self.buffer = buffer;
        self.stream.set_buffer(buffer);
    }

    fn push_chunk(&mut self, start: Micros, pcm: &[u8], server_now: Micros) {
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

pub enum Event {
    /// The device of an open output went away
    Lost(String),
    #[cfg(windows)]
    DefaultChanged,
    /// A device was added or removed, or switched on or off
    #[cfg(windows)]
    DevicesChanged,
    /// The outputs chosen to play besides the default, by name
    Chosen(Vec<String>),
}

/// An output playing the stream; dropping it closes the sound card's stream and stops feeding it
struct Open {
    name: String,
    feed: Arc<Mutex<Feed>>,
    shared: Arc<Mutex<Shared>>,
    _stream: cpal::Stream,
}

impl Drop for Open {
    fn drop(&mut self) {
        self.feed.lock().detach(&self.shared);
    }
}

/// Play the stream on the default output and the chosen ones for as long as the process runs.
/// Only the default output feeds the visualization. `default` is kept at the default output's name.
#[allow(clippy::too_many_arguments)]
pub fn run(
    feed: &Arc<Mutex<Feed>>,
    clock: &Arc<Clock>,
    mute: &Arc<Mute>,
    played: Option<&flume::Sender<(u32, Vec<f32>)>>,
    events: &flume::Sender<Event>,
    changes: &flume::Receiver<Event>,
    default: &Mutex<Option<String>>,
    mut chosen: Vec<String>,
) {
    #[cfg(windows)]
    let _watch = default_device::Watch::register(events.clone())
        .inspect_err(|err| {
            tracing::warn!("Failed to watch for audio device changes: {err:#}");
        })
        .ok();
    let mut main: Option<Open> = None;
    let mut extras: Vec<Open> = Vec::new();
    loop {
        let host = cpal::default_host();
        let default_device = host.default_output_device();
        let default_name = default_device
            .as_ref()
            .and_then(|device| device.name().ok());
        default.lock().clone_from(&default_name);
        let mut retry = false;
        let mut failures = Vec::new();

        if main.is_none() {
            match default_device
                .context("no audio output device")
                .and_then(|device| open(&device, feed, clock, mute, played.cloned(), events))
            {
                Ok(output) => main = Some(output),
                Err(err) => {
                    tracing::warn!("Failed to open an audio output: {err:#}");
                    failures.push(format!(
                        "{} ({})",
                        default_name.as_deref().unwrap_or("the default speaker"),
                        crate::problem::reason(&err)
                    ));
                    retry = true;
                }
            }
        }

        // a chosen output that is now the default plays once, as the default
        extras.retain(|output| {
            chosen.contains(&output.name) && Some(&output.name) != default_name.as_ref()
        });
        let missing: Vec<&String> = chosen
            .iter()
            .filter(|name| {
                Some(*name) != default_name.as_ref()
                    && !extras.iter().any(|output| &output.name == *name)
            })
            .collect();
        if !missing.is_empty() {
            match host.output_devices() {
                Ok(devices) => {
                    for device in devices {
                        if !device.name().is_ok_and(|name| missing.contains(&&name)) {
                            continue;
                        }
                        let name = device.name().unwrap_or_default();
                        match open(&device, feed, clock, mute, None, events) {
                            Ok(output) => extras.push(output),
                            Err(err) => {
                                tracing::warn!("Failed to open a chosen audio output: {err:#}");
                                failures.push(format!("{name} ({})", crate::problem::reason(&err)));
                                retry = true;
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!("Failed to list the audio outputs: {err:#}");
                    failures.push(format!(
                        "the chosen speakers ({})",
                        crate::problem::reason(&anyhow::Error::from(err))
                    ));
                    retry = true;
                }
            }
        }
        if failures.is_empty() {
            crate::problem::clear(crate::problem::Kind::Output);
        } else {
            crate::problem::set(
                crate::problem::Kind::Output,
                format!("Can't play on {}", failures.join(", ")),
            );
        }
        // without Windows' device notices, a chosen output that isn't there is looked for again
        let retry = retry || (cfg!(not(windows)) && !missing.is_empty());

        let first = if retry {
            match changes.recv_timeout(REOPEN_INTERVAL) {
                Ok(event) => Some(event),
                Err(flume::RecvTimeoutError::Timeout) => None,
                Err(flume::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match changes.recv() {
                Ok(event) => Some(event),
                Err(_) => return,
            }
        };
        for event in first.into_iter().chain(changes.try_iter()) {
            match event {
                Event::Lost(name) => {
                    tracing::warn!("The audio output {name} went away");
                    if main.as_ref().is_some_and(|output| output.name == name) {
                        main = None;
                    }
                    extras.retain(|output| output.name != name);
                }
                #[cfg(windows)]
                Event::DefaultChanged => {
                    tracing::info!("The default audio output device changed; reopening on it");
                    main = None;
                }
                #[cfg(windows)]
                Event::DevicesChanged => {}
                Event::Chosen(names) => chosen = names,
            }
        }
    }
}

/// The names of this PC's audio outputs
pub fn names() -> Vec<String> {
    match cpal::default_host().output_devices() {
        Ok(devices) => devices.filter_map(|device| device.name().ok()).collect(),
        Err(err) => {
            tracing::warn!("Failed to list the audio outputs: {err:#}");
            Vec::new()
        }
    }
}

fn open(
    device: &cpal::Device,
    feed: &Arc<Mutex<Feed>>,
    clock: &Arc<Clock>,
    mute: &Arc<Mute>,
    played: Option<flume::Sender<(u32, Vec<f32>)>>,
    events: &flume::Sender<Event>,
) -> anyhow::Result<Open> {
    let name = device
        .name()
        .unwrap_or_else(|_| "[unknown name]".to_string());
    let config = device
        .default_output_config()
        .with_context(|| format!("read the format of {name}"))?;
    tracing::info!("Using audio device: {name} ({config:?})");
    let rate = config.sample_rate().0;
    let shared = feed.lock().output(rate);
    let callback = Callback {
        shared: Arc::clone(&shared),
        clock: Arc::clone(clock),
        mute: Arc::clone(mute),
        played,
        rate,
        device_channels: usize::from(config.channels()),
        buf: Vec::new(),
    };
    let events = events.clone();
    let lost = name.clone();
    let on_error = move |err| {
        tracing::warn!("Audio stream error on {lost}: {err}");
        if matches!(err, cpal::StreamError::DeviceNotAvailable) {
            let _ = events.send(Event::Lost(lost.clone()));
        }
    };
    let stream_config = config.config();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(device, &stream_config, callback, on_error),
        cpal::SampleFormat::I16 => build::<i16>(device, &stream_config, callback, on_error),
        cpal::SampleFormat::I32 => build::<i32>(device, &stream_config, callback, on_error),
        cpal::SampleFormat::U16 => build::<u16>(device, &stream_config, callback, on_error),
        other => anyhow::bail!("unsupported sample format {other}"),
    }?;
    stream.play()?;
    feed.lock().attach(&shared);
    Ok(Open {
        name,
        feed: Arc::clone(feed),
        shared,
        _stream: stream,
    })
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

    /// A registration for Windows' notices that the default output device changed or a device
    /// came or went. It stays registered until dropped.
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
                tracing::warn!("Failed to stop watching the audio devices: {err:#}");
            }
        }
    }

    /// Called by Windows on its own thread, so it only passes the notices on
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
            let _ = self.events.send(Event::DevicesChanged);
            Ok(())
        }

        fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> {
            let _ = self.events.send(Event::DevicesChanged);
            Ok(())
        }

        fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> {
            let _ = self.events.send(Event::DevicesChanged);
            Ok(())
        }

        fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> Result<()> {
            Ok(())
        }
    }
}
