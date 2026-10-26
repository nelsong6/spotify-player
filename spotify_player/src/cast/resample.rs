//! Converts the stream's sample rate to the output device's. Windows' shared-mode output only takes
//! the device's own rate (often 48 kHz) while Spotify's audio is 44.1 kHz. Each output sample is a
//! 4-point Hermite interpolation of the input; going up in rate this adds nothing audible.

pub struct Resampler {
    channels: usize,
    /// Input frames per output frame
    step: f64,
    /// Interleaved input not yet used up, starting one frame before the one at `pos`
    pending: Vec<f32>,
    /// Position of the next output frame, in input frames from the start of `pending`
    pos: f64,
}

impl Resampler {
    pub fn new(channels: usize, in_rate: u32, out_rate: u32) -> Self {
        Self {
            channels,
            step: f64::from(in_rate) / f64::from(out_rate),
            // one silent frame of history, so the first input frame has a frame before it
            pending: vec![0.0; channels],
            pos: 1.0,
        }
    }

    /// Where the next output frame sits against the next input frame passed to `process`, in
    /// input frames; negative when it comes from input already passed in.
    pub fn next_offset(&self) -> f64 {
        self.pos - (self.pending.len() / self.channels) as f64
    }

    /// Resample `input` (interleaved), appending to `out`. Output is continuous across calls.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let ch = self.channels;
        self.pending.extend_from_slice(input);
        let frames = self.pending.len() / ch;
        while (self.pos as usize) + 2 < frames {
            let i = self.pos as usize;
            let t = (self.pos - i as f64) as f32;
            for c in 0..ch {
                let at = |k: usize| self.pending[k * ch + c];
                out.push(hermite(at(i - 1), at(i), at(i + 1), at(i + 2), t));
            }
            self.pos += self.step;
        }
        let used = (self.pos as usize).saturating_sub(1).min(frames);
        self.pending.drain(..used * ch);
        self.pos -= used as f64;
    }
}

fn hermite(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * t + c2) * t + c1) * t + x0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f32, frames: usize, start: usize) -> Vec<f32> {
        (start..start + frames)
            .flat_map(|n| {
                let v = (2.0 * std::f32::consts::PI * freq * n as f32 / rate as f32).sin();
                [v, -v]
            })
            .collect()
    }

    #[test]
    fn same_rate_passes_samples_through() {
        let mut r = Resampler::new(2, 44_100, 44_100);
        let input = sine(44_100, 440.0, 1000, 0);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        // two frames of lookahead stay behind
        assert_eq!(out.len(), input.len() - 2 * 2);
        assert_eq!(out, input[..out.len()]);
    }

    #[test]
    fn upsamples_a_tone_in_pieces_without_error_or_drift() {
        let mut r = Resampler::new(2, 44_100, 48_000);
        let mut out = Vec::new();
        // uneven pieces, as chunks arrive from the network
        let mut start = 0;
        for len in [100, 882, 1, 3, 2000, 441, 5000] {
            r.process(&sine(44_100, 1000.0, len, start), &mut out);
            start += len;
        }
        let frames = out.len() / 2;
        let expected_frames = (start as f64 * 48_000.0 / 44_100.0) as usize;
        assert!(
            frames.abs_diff(expected_frames) <= 3,
            "{frames} vs {expected_frames}"
        );
        // output frame k is the input at k / 48000 s, both channels
        let expected = sine(48_000, 1000.0, frames, 0);
        let worst = out
            .iter()
            .zip(&expected)
            .skip(4)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 2e-3, "worst error {worst}");
    }
}
