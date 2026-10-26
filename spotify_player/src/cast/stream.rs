//! Plays the received chunks at their time: snapclient's `Stream::getPlayerChunk`. A chunk
//! recorded at server time `t` reaches the speaker at `t + buffer`. A sample's "age" is how late it
//! is against that. Large errors are fixed at once by skipping audio or playing silence (hard
//! sync). Small ones are fixed by dropping or repeating one frame every so often (soft sync),
//! which absorbs the drift between the NAS's clock and the sound card's.

use super::protocol::Micros;
use std::collections::VecDeque;

/// Soft sync starts when the short median is this late or early
const CORRECTION_BEGIN: Micros = 100;
/// Chunks this much older than the buffer are no use and are dropped on arrival
const TOO_OLD: Micros = 5_000_000;

struct Chunk {
    /// Server time of the chunk's first frame
    start: Micros,
    /// Interleaved samples at the output rate
    samples: Vec<f32>,
    /// Frames already played
    read: usize,
}

/// The median of the last `size` values
struct Medians {
    size: usize,
    values: VecDeque<Micros>,
}

impl Medians {
    fn new(size: usize) -> Self {
        Self {
            size,
            values: VecDeque::with_capacity(size),
        }
    }

    fn add(&mut self, v: Micros) {
        if self.values.len() == self.size {
            self.values.pop_front();
        }
        self.values.push_back(v);
    }

    fn is_full(&self) -> bool {
        self.values.len() == self.size
    }

    fn median(&self) -> Micros {
        let mut sorted: Vec<_> = self.values.iter().copied().collect();
        sorted.sort_unstable();
        sorted.get(sorted.len() / 2).copied().unwrap_or_default()
    }

    fn clear(&mut self) {
        self.values.clear();
    }
}

pub struct Stream {
    rate: u32,
    channels: usize,
    chunks: VecDeque<Chunk>,
    /// How far behind the server's recording the speakers play
    buffer: Micros,
    hard_sync: bool,
    long: Medians,
    short: Medians,
    mini: Medians,
    long_median: Micros,
    short_median: Micros,
    /// Frames played since the medians were last refreshed
    since_stats: usize,
    /// Every this many frames, one frame is dropped (positive) or repeated (negative); 0 for none
    correct_after: i64,
    since_correction: i64,
    scratch: Vec<f32>,
    pub hard_syncs: u32,
}

impl Stream {
    pub fn new(rate: u32, channels: usize) -> Self {
        Self {
            rate,
            channels,
            chunks: VecDeque::new(),
            buffer: 1_000_000,
            hard_sync: true,
            long: Medians::new(500),
            short: Medians::new(100),
            mini: Medians::new(20),
            long_median: 0,
            short_median: 0,
            since_stats: 0,
            correct_after: 0,
            since_correction: 0,
            scratch: Vec::new(),
            hard_syncs: 0,
        }
    }

    pub fn set_buffer(&mut self, buffer: Micros) {
        self.buffer = buffer.max(0);
    }

    /// Drop everything queued, for a new stream
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.hard_sync = true;
        self.reset_medians();
    }

    pub fn push(&mut self, start: Micros, samples: Vec<f32>, server_now: Micros) {
        if server_now - start > TOO_OLD + self.buffer {
            return;
        }
        self.chunks.push_back(Chunk {
            start,
            samples,
            read: 0,
        });
        while self
            .chunks
            .front()
            .is_some_and(|c| server_now - c.start > TOO_OLD + self.buffer)
        {
            self.chunks.pop_front();
        }
    }

    fn frames_to_micros(&self, frames: usize) -> Micros {
        (frames as i64 * 1_000_000) / i64::from(self.rate)
    }

    fn micros_to_frames(&self, us: Micros) -> usize {
        (us.max(0) * i64::from(self.rate) / 1_000_000) as usize
    }

    /// Server time of the next frame to play
    fn head_time(&self) -> Option<Micros> {
        self.chunks
            .front()
            .map(|c| c.start + self.frames_to_micros(c.read))
    }

    fn available_frames(&self) -> usize {
        self.chunks
            .iter()
            .map(|c| c.samples.len() / self.channels - c.read)
            .sum()
    }

    /// Copy the next `frames` frames into `out`; false when the queue runs out first
    fn read_frames(&mut self, out: &mut [f32], frames: usize) -> bool {
        let ch = self.channels;
        let mut done = 0;
        while done < frames {
            let Some(chunk) = self.chunks.front_mut() else {
                out[done * ch..frames * ch].fill(0.0);
                return false;
            };
            let left = chunk.samples.len() / ch - chunk.read;
            let n = left.min(frames - done);
            out[done * ch..(done + n) * ch]
                .copy_from_slice(&chunk.samples[chunk.read * ch..(chunk.read + n) * ch]);
            chunk.read += n;
            done += n;
            if chunk.read * ch == chunk.samples.len() {
                self.chunks.pop_front();
            }
        }
        true
    }

    fn skip_frames(&mut self, mut frames: usize) {
        while frames > 0 {
            let Some(chunk) = self.chunks.front_mut() else {
                return;
            };
            let left = chunk.samples.len() / self.channels - chunk.read;
            let n = left.min(frames);
            chunk.read += n;
            frames -= n;
            if n == left {
                self.chunks.pop_front();
            }
        }
    }

    fn reset_medians(&mut self) {
        self.long.clear();
        self.short.clear();
        self.mini.clear();
        self.long_median = 0;
        self.short_median = 0;
        self.correct_after = 0;
        self.since_correction = 0;
    }

    fn start_hard_sync(&mut self, why: &str) {
        tracing::debug!("Cast: lining up again ({why})");
        self.hard_sync = true;
        self.hard_syncs += 1;
    }

    /// Fill `out` (interleaved, `out.len() / channels` frames) with what should reach the speaker
    /// `dac` microseconds from now, when the server's clock reads `server_now`. Returns whether
    /// any of it is audio rather than silence.
    pub fn fill(&mut self, out: &mut [f32], server_now: Micros, dac: Micros) -> bool {
        let frames = out.len() / self.channels;
        if dac > self.buffer || frames == 0 {
            out.fill(0.0);
            return false;
        }
        let Some(head) = self.head_time() else {
            // a gap (the session paused or the network stalled): line up again on what comes next
            out.fill(0.0);
            if !self.hard_sync {
                self.start_hard_sync("the queue ran dry");
            }
            return false;
        };
        let buffer = self.buffer;
        let age_of = |head: Micros| server_now - head - buffer + dac;

        if self.hard_sync {
            let duration = self.frames_to_micros(frames);
            let mut age = age_of(head);
            if age < -duration {
                out.fill(0.0);
                return false;
            }
            if age > 0 {
                // too late: skip ahead to the frame that is due now
                self.skip_frames(self.micros_to_frames(age));
                let Some(head) = self.head_time() else {
                    out.fill(0.0);
                    return false;
                };
                age = age_of(head).min(0);
            }
            let silent = self.micros_to_frames(-age).min(frames);
            if self.available_frames() < frames - silent {
                out.fill(0.0);
                return false;
            }
            out[..silent * self.channels].fill(0.0);
            self.read_frames(&mut out[silent * self.channels..], frames - silent);
            self.hard_sync = false;
            self.reset_medians();
            return true;
        }

        // soft sync: read one frame more or less every `correct_after` frames
        let mut correction = 0i64;
        if self.correct_after != 0 {
            self.since_correction += frames as i64;
            if self.since_correction >= self.correct_after.abs() {
                correction = self.since_correction / self.correct_after;
                self.since_correction %= self.correct_after.abs();
            }
        }
        let to_read = (frames as i64 + correction).max(1) as usize;
        let ch = self.channels;
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.resize(to_read * ch, 0.0);
        let complete = self.read_frames(&mut scratch, to_read);
        // spread the dropped or repeated frames evenly over the buffer
        for j in 0..frames {
            let src = j * to_read / frames;
            out[j * ch..(j + 1) * ch].copy_from_slice(&scratch[src * ch..(src + 1) * ch]);
        }
        self.scratch = scratch;
        if !complete {
            self.start_hard_sync("the queue ran dry mid-buffer");
            return true;
        }

        let age = age_of(head);
        self.correct_after = 0;
        if (self.long.is_full() && self.long_median.abs() > 2_000 && age.abs() > 500)
            || (self.short.is_full() && self.short_median.abs() > 5_000 && age.abs() > 500)
            || (self.mini.is_full() && self.mini.median().abs() > 50_000 && age.abs() > 500)
            || age.abs() > 500_000
        {
            self.start_hard_sync(&format!(
                "age {age} us, medians {} / {} / {} us",
                self.mini.median(),
                self.short_median,
                self.long_median
            ));
        } else if self.short.is_full() {
            let mini = self.mini.median();
            // like snapclient: up to 0.05% faster or slower, more the further off
            let rate_off = |median: Micros| (median as f64 / 100.0 * 0.000_05).min(0.000_5);
            if self.short_median > CORRECTION_BEGIN && mini > 50 && age > 50 {
                self.correct_after = (1.0 / rate_off(self.short_median)).round() as i64;
            } else if self.short_median < -CORRECTION_BEGIN && mini < -50 && age < -50 {
                self.correct_after = -(1.0 / rate_off(-self.short_median)).round() as i64;
            }
        }
        self.long.add(age);
        self.short.add(age);
        self.mini.add(age);
        self.since_stats += frames;
        if self.since_stats >= self.rate as usize {
            self.since_stats = 0;
            self.long_median = self.long.median();
            self.short_median = self.short.median();
            tracing::debug!(
                "Cast sync: age {age} us, medians {} / {} / {} us, correcting every {} frames, {} hard syncs, dac {dac} us",
                self.mini.median(),
                self.short_median,
                self.long_median,
                self.correct_after,
                self.hard_syncs
            );
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const CHUNK_MS: i64 = 20;
    const PERIOD: usize = 480;

    /// A sample's value encodes the server time it was recorded at, in microseconds
    fn chunk(start: Micros) -> Vec<f32> {
        let frames = (RATE as i64 * CHUNK_MS / 1000) as usize;
        (0..frames)
            .flat_map(|n| {
                let t = (start + n as i64 * 1_000_000 / i64::from(RATE)) as f32;
                [t, t]
            })
            .collect()
    }

    /// Plays `secs` of audio through a sound card whose clock runs `drift_ppm` off the server's,
    /// with callbacks that wake up to `jitter` early. Returns, per callback after the first
    /// `settle` seconds, how late its first frame reaches the speaker, and the hard syncs then.
    fn simulate(drift_ppm: f64, jitter: Micros, secs: i64, settle: i64) -> (Vec<Micros>, u32) {
        let buffer = 400_000;
        let base_dac = 15_000;
        let mut stream = Stream::new(RATE, 2);
        stream.set_buffer(buffer);
        let mut next_chunk: Micros = 0;
        let mut errors = Vec::new();
        let mut syncs_at_settle = None;
        let mut out = vec![0.0f32; PERIOD * 2];
        let mut seed = 7u64;
        let periods = secs as usize * RATE as usize / PERIOD;
        for n in 0..periods {
            // the device's playback instants are regular on its own clock
            let device_time = (n * PERIOD) as f64 / f64::from(RATE) * 1e6;
            let playback = (device_time * (1.0 + drift_ppm * 1e-6)) as Micros + 1_000_000;
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let wake_early = (seed >> 33) as i64 % (jitter + 1);
            let callback = playback - base_dac - wake_early;
            // the server sends what it records right away
            while next_chunk <= callback {
                stream.push(next_chunk, chunk(next_chunk), callback);
                next_chunk += CHUNK_MS * 1000;
            }
            let played = stream.fill(&mut out, callback, playback - callback);
            if playback >= settle * 1_000_000 + 1_000_000 {
                syncs_at_settle.get_or_insert(stream.hard_syncs);
                if played && out[0] != 0.0 {
                    // the first frame was recorded at out[0] and plays at `playback`
                    errors.push(playback - (out[0] as Micros + buffer));
                }
            }
        }
        (errors, stream.hard_syncs - syncs_at_settle.unwrap_or(0))
    }

    fn worst_median(errors: &[Micros]) -> Micros {
        errors
            .chunks(100)
            .map(|w| {
                let mut w = w.to_vec();
                w.sort_unstable();
                w[w.len() / 2].abs()
            })
            .max()
            .unwrap()
    }

    #[test]
    fn plays_in_time_on_a_perfect_clock() {
        let (errors, syncs) = simulate(0.0, 0, 20, 2);
        assert_eq!(syncs, 0);
        assert!(worst_median(&errors) < 200, "{}", worst_median(&errors));
    }

    #[test]
    fn follows_a_drifting_jittery_sound_card() {
        for drift in [-120.0, 80.0] {
            let (errors, syncs) = simulate(drift, 3_000, 120, 20);
            assert_eq!(syncs, 0, "drift {drift}");
            let worst = worst_median(&errors);
            // the frame stamps are exact to the microsecond, so this is the real lateness
            assert!(worst < 1_000, "drift {drift}: worst median {worst} us");
        }
    }

    #[test]
    fn lines_up_again_after_a_gap() {
        let mut stream = Stream::new(RATE, 2);
        stream.set_buffer(400_000);
        let mut out = vec![0.0f32; PERIOD * 2];
        stream.push(0, chunk(0), 0);
        assert!(!stream.fill(&mut out, 0, 10_000), "too early to play");
        assert!(stream.fill(&mut out, 400_000 - 10_000, 10_000));
        // the queue runs dry, then a chunk arrives from much later
        while stream.fill(&mut out, 400_000, 10_000) {}
        stream.push(5_000_000, chunk(5_000_000), 5_000_000);
        assert!(!stream.fill(&mut out, 5_000_000, 10_000));
        assert!(stream.fill(&mut out, 5_390_000, 10_000));
        assert_eq!(out[0] as Micros, 5_000_000);
    }
}
