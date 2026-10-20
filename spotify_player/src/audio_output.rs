//! Audio output that survives its device going away. librespot's rodio sink keeps the stream it
//! opened at startup: when that device is unplugged or drops off USB, the stream's error is
//! printed over the terminal and the sink waits forever for its dead queue to drain. This sink
//! logs the error instead and reopens on the current default device.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};
use rodio::cpal::traits::{DeviceTrait, HostTrait};

/// How long to wait between attempts to open an output while there is none
const REOPEN_INTERVAL: Duration = Duration::from_secs(2);
/// Queued chunks to hold before waiting for the device to play some (about half a second)
const MAX_QUEUED_CHUNKS: usize = 26;

struct Output {
    sink: rodio::Sink,
    _stream: rodio::OutputStream,
}

pub struct RecoveringSink {
    output: Option<Output>,
    /// Set by the stream's error callback when its device is no longer available
    device_lost: Arc<AtomicBool>,
    last_open_attempt: Option<Instant>,
}

impl RecoveringSink {
    pub fn open() -> Self {
        let mut sink = Self {
            output: None,
            device_lost: Arc::new(AtomicBool::new(false)),
            last_open_attempt: None,
        };
        sink.ensure_output();
        sink
    }

    fn is_device_lost(&self) -> bool {
        self.device_lost.load(Ordering::SeqCst)
    }

    /// Drop an output whose device is gone, and open one on the default device when there is
    /// none, at most every `REOPEN_INTERVAL`.
    fn ensure_output(&mut self) {
        if self.output.is_some() && self.is_device_lost() {
            tracing::warn!("The audio output device went away; reopening on the default device");
            self.output = None;
        }
        if self.output.is_some()
            || self
                .last_open_attempt
                .is_some_and(|t| t.elapsed() < REOPEN_INTERVAL)
        {
            return;
        }
        self.last_open_attempt = Some(Instant::now());
        match open_output(&self.device_lost) {
            Ok(output) => self.output = Some(output),
            Err(err) => tracing::warn!("Failed to open an audio output: {err:#}"),
        }
    }

    /// Wait while the device plays queued audio, until at most `max_queued` chunks are left
    fn wait_for_queue(&self, max_queued: usize) {
        if let Some(output) = &self.output {
            while output.sink.len() > max_queued && !self.is_device_lost() {
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn open_output(device_lost: &Arc<AtomicBool>) -> anyhow::Result<Output> {
    let device = rodio::cpal::default_host()
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no audio output device"))?;
    tracing::info!(
        "Using audio device: {}",
        device
            .name()
            .unwrap_or_else(|_| "[unknown name]".to_string())
    );

    device_lost.store(false, Ordering::SeqCst);
    let lost = Arc::clone(device_lost);
    let mut stream = rodio::OutputStreamBuilder::from_device(device)?
        .with_error_callback(move |err| {
            tracing::warn!("Audio stream error: {err}");
            if matches!(err, rodio::cpal::StreamError::DeviceNotAvailable) {
                lost.store(true, Ordering::SeqCst);
            }
        })
        .open_stream_or_fallback()?;
    stream.log_on_drop(false);

    let sink = rodio::Sink::connect_new(stream.mixer());
    Ok(Output {
        sink,
        _stream: stream,
    })
}

impl Sink for RecoveringSink {
    fn start(&mut self) -> SinkResult<()> {
        self.ensure_output();
        if let Some(output) = &self.output {
            output.sink.play();
        }
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.wait_for_queue(0);
        if let Some(output) = &self.output {
            output.sink.pause();
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet
            .samples()
            .map_err(|err| SinkError::OnWrite(err.to_string()))?;
        self.ensure_output();
        let Some(output) = &self.output else {
            // nothing to play on: let the audio's time pass so the track keeps its pace
            let secs = samples.len() as f64 / f64::from(u32::from(NUM_CHANNELS) * SAMPLE_RATE);
            thread::sleep(Duration::from_secs_f64(secs));
            return Ok(());
        };

        let samples_f32: &[f32] = &converter.f64_to_f32(samples);
        output.sink.append(rodio::buffer::SamplesBuffer::new(
            rodio::ChannelCount::from(NUM_CHANNELS),
            SAMPLE_RATE,
            samples_f32,
        ));
        self.wait_for_queue(MAX_QUEUED_CHUNKS);
        Ok(())
    }
}
