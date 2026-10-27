use super::config;
use super::utils::construct_and_render_styled_block;
use crate::state::SharedState;
use crate::state::UIStateGuard;
use parking_lot::Mutex;
use ratatui::{layout::Rect, style::Color, widgets::Borders, Frame};
use rustfft::{num_complex::Complex, FftPlanner};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

const FFT_SIZE: usize = 1024;
/// Number of new samples consumed per FFT frame (overlap = `FFT_SIZE` - `HOP_SIZE`).
/// At 44100 Hz: 128 samples ≈ 2.9 ms between updates.
const HOP_SIZE: usize = 128;
pub const NUM_BANDS: usize = 128;

/// Per-FFT-frame decay multiplier for individual bands.
/// At 44100 Hz / `HOP_SIZE` 128 ≈ 344 hops/s, 0.985^~151 ≈ 1% in ~0.44 s — snappy
/// enough to track transients, not so slow that it smears.
const DECAY_FACTOR: f32 = 0.985;
/// Slower decay for the peak envelope used for normalization. At 344 hops/s,
/// 0.9985^x = 0.01 → x ≈ 1535 hops → ~4.5 s. The envelope stays elevated
/// through quiet passages so the bars reflect genuine relative loudness instead
/// of always filling to 100%.
const DECAY_FACTOR_PEAK: f32 = 0.9985;
/// Reference sample rate used by the **render-side** decay helpers
/// (`decay_for_elapsed`, `peak_decay_for_elapsed`).
/// The analyzer uses its own `Analyzer::sample_rate` field so that decay timings
/// stay precise at the output device's rate (often 48 000 Hz).
const SAMPLE_RATE: f32 = 44_100.0;
/// Bar body characters from the base up, thinning toward the top like the cover's ramp.
const BAR_BODY: &[u8] = b"@%#*+=-:..";
/// How fast a peak marker sinks, in full bar heights per second.
const PEAK_FALL_PER_SEC: f32 = 0.6;

/// Shared frequency-band state exposed between the audio sink and the UI.
/// Storing `updated_at` lets the render function apply smooth time-based decay
/// independent of how often `Analyzer::push()` is called.
pub struct VisBands {
    /// Fixed-size array of per-band magnitudes.
    /// Using `[f32; NUM_BANDS]` instead of `Vec<f32>` means the render-frame
    /// copy (`let values = guard.values;`) is a plain stack copy with no heap
    /// allocation.
    pub values: [f32; NUM_BANDS],
    /// Wall-clock timestamp of the last `push()` hop that updated `values`.
    /// The render function reads this to compute time-based inter-frame decay
    /// without needing to be called at a fixed rate.
    pub updated_at: Instant,
    /// Slow-decaying peak envelope used to normalise bar heights.
    /// Rises instantly to any louder value; decays with `DECAY_FACTOR_PEAK`.
    /// Kept separate from per-band values so quiet passages look genuinely
    /// quieter — the VU «breathes» with the music.
    pub peak_envelope: f32,
    /// Set while audio plays here and cleared by `Analyzer::stop()`. The UI draws no
    /// bars while it is unset.
    pub is_active: bool,
    /// Whether the visualization is shown, toggled while the app runs. When off, the
    /// analyzer skips the FFT.
    pub enabled: bool,
    /// Each band's recent peak level, kept by the render side for the peak markers.
    pub peaks: [f32; NUM_BANDS],
    /// When `peaks` last sank.
    pub peaks_at: Instant,
}

impl VisBands {
    pub fn new() -> Self {
        Self {
            values: [0.0f32; NUM_BANDS],
            updated_at: Instant::now(),
            peak_envelope: 1e-6,
            is_active: false,
            enabled: crate::config::get_config()
                .app_config
                .enable_audio_visualization,
            peaks: [0.0f32; NUM_BANDS],
            peaks_at: Instant::now(),
        }
    }
}

impl Default for VisBands {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns the compound `DECAY_FACTOR` multiplier for the given elapsed wall-clock
/// duration.
///
/// Used **only on the render side** (`render_audio_visualization`) to interpolate
/// bar heights smoothly between audio-sink updates. It uses the fixed `SAMPLE_RATE`
/// reference (44 100 Hz); the audio sink inlines its own calculation using
/// `Analyzer::sample_rate` so both sides are independently accurate.
pub fn decay_for_elapsed(elapsed: std::time::Duration) -> f32 {
    let elapsed_hops = elapsed.as_secs_f32() * SAMPLE_RATE / HOP_SIZE as f32;
    DECAY_FACTOR.powf(elapsed_hops)
}

/// Returns the compound `DECAY_FACTOR_PEAK` multiplier for the given elapsed
/// wall-clock duration.
///
/// Used **only on the render side** to decay the peak-envelope estimate between
/// audio packets. See `decay_for_elapsed` for the render-vs-sink split.
pub fn peak_decay_for_elapsed(elapsed: std::time::Duration) -> f32 {
    let elapsed_hops = elapsed.as_secs_f32() * SAMPLE_RATE / HOP_SIZE as f32;
    DECAY_FACTOR_PEAK.powf(elapsed_hops)
}

/// Computes real-time FFT frequency bands from the audio as it plays and exposes
/// them via a shared buffer for the UI.
pub struct Analyzer {
    /// Ring-buffer of mono f32 samples waiting to be processed.
    /// `VecDeque` gives O(1) front-drain instead of Vec's O(remaining) shift.
    sample_buf: VecDeque<f32>,
    /// Shared state written every hop and read by the UI render thread.
    /// Guarded by a `Mutex`; the render path uses `try_lock()` to avoid
    /// blocking the audio thread.
    bands: Arc<Mutex<VisBands>>,
    /// Forward FFT plan reused every hop — `rustfft` plans are thread-safe
    /// and allocation-free once created.
    fft: Arc<dyn rustfft::Fft<f32>>,
    /// Precomputed Hann window coefficients — computed once in `new()` and
    /// reused every hop to avoid repeated `cos()` calls in the hot path.
    hann_window: Vec<f32>,
    /// Reusable FFT input buffer — avoids a `Vec` allocation per hop.
    fft_buf: Vec<Complex<f32>>,
    /// Reusable magnitude buffer — avoids a `Vec` allocation per hop.
    magnitudes: Vec<f32>,
    /// Actual audio sample rate in Hz — used for precise hop-based decay
    /// calculation, since the output device can run at 44100 or 48000 Hz.
    sample_rate: f32,
    /// Precomputed (start, end) bin-index ranges for each log-scale band.
    /// Computed once in `new()` so `push()` never runs `powf` per hop.
    band_ranges: Vec<(usize, usize)>,
    /// Reusable output buffer for `fill_log_bands` — no `Vec` allocation per hop.
    new_bands: [f32; NUM_BANDS],
    /// Scratch buffer for `smooth_bands` — avoids `to_vec()` allocation per hop.
    smooth_scratch: [f32; NUM_BANDS],
}

impl Analyzer {
    /// `sample_rate` is the rate of the audio passed to `push`, so that hop-based
    /// decay timings are accurate.
    pub fn new(bands: Arc<Mutex<VisBands>>, sample_rate: f32) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let hann_window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            })
            .collect();
        let band_ranges = precompute_band_ranges(FFT_SIZE / 2, NUM_BANDS);
        Self {
            sample_buf: VecDeque::with_capacity(FFT_SIZE * 2),
            bands,
            fft,
            hann_window,
            fft_buf: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            magnitudes: vec![0.0; FFT_SIZE / 2],
            sample_rate,
            band_ranges,
            new_bands: [0.0f32; NUM_BANDS],
            smooth_scratch: [0.0f32; NUM_BANDS],
        }
    }

    /// Zero out the bands and reset normalization when the audio stops, so the
    /// bars fall to silence and the next playback starts with a fresh baseline.
    pub fn stop(&mut self) {
        let mut g = self.bands.lock();
        g.values.fill(0.0);
        g.peak_envelope = 1e-6;
        g.updated_at = Instant::now();
        g.is_active = false;
        g.peaks.fill(0.0);
        drop(g);
        self.sample_buf.clear();
    }

    /// Take in the interleaved stereo samples just played.
    pub fn push(&mut self, samples: &[f32]) {
        let mut g = self.bands.lock();
        if !g.enabled {
            drop(g);
            self.sample_buf.clear();
            return;
        }
        g.is_active = true;
        drop(g);
        // Samples are interleaved stereo (L, R, L, R, …); mix down to mono.
        self.sample_buf.extend(samples.chunks(2).map(|c| {
            if c.len() == 2 {
                f32::midpoint(c[0], c[1])
            } else {
                c[0]
            }
        }));

        // Update vis_bands after EVERY hop (not at the end of the batch).
        //
        // Batching reduces mutex contention but delays the first update by the
        // full packet duration (~46 ms for a 2048-sample packet). With per-hop
        // updates a transient at the START of a packet is visible within one hop
        // (~2.9 ms) + render delay (~32 ms) instead of ~78 ms.
        //
        // Decay correctness is preserved by using wall-clock elapsed from
        // vis_bands.updated_at: hops within the same push() call are ~3 ms apart,
        // so DECAY_FACTOR ^ (~1 hop elapsed) ≈ 0.985 ≈ 1.0 — no peak smearing.
        while self.sample_buf.len() >= FFT_SIZE {
            // Fill fft_buf using as_slices() to avoid make_contiguous()'s
            // potential O(n) rotation, and reuse the preallocated buffer.
            {
                let (front, back) = self.sample_buf.as_slices();
                if front.len() >= FFT_SIZE {
                    for (dst, (&s, &w)) in self
                        .fft_buf
                        .iter_mut()
                        .zip(front.iter().zip(self.hann_window.iter()))
                    {
                        *dst = Complex::new(s * w, 0.0);
                    }
                } else {
                    let split = front.len();
                    for (dst, (&s, &w)) in self.fft_buf[..split]
                        .iter_mut()
                        .zip(front.iter().zip(self.hann_window[..split].iter()))
                    {
                        *dst = Complex::new(s * w, 0.0);
                    }
                    let remaining = FFT_SIZE - split;
                    for (dst, (&s, &w)) in self.fft_buf[split..].iter_mut().zip(
                        back[..remaining]
                            .iter()
                            .zip(self.hann_window[split..].iter()),
                    ) {
                        *dst = Complex::new(s * w, 0.0);
                    }
                }
            }

            self.fft.process(&mut self.fft_buf);

            // Compute magnitudes in place — no allocation per hop.
            for (mag, c) in self.magnitudes.iter_mut().zip(self.fft_buf.iter()) {
                *mag = c.norm();
            }

            // Fill pre-allocated band buffers in-place — no Vec allocation per hop.
            fill_log_bands(&self.magnitudes, &self.band_ranges, &mut self.new_bands);
            smooth_bands(&mut self.new_bands, &mut self.smooth_scratch);

            // Apply wall-clock decay since the last hop, then rise to any louder value.
            // Use self.sample_rate for precision (may be 44100 or 48000 Hz).
            let mut g = self.bands.lock();
            let elapsed_hops =
                g.updated_at.elapsed().as_secs_f32() * self.sample_rate / HOP_SIZE as f32;
            let decay = DECAY_FACTOR.powf(elapsed_hops);
            let peak_decay = DECAY_FACTOR_PEAK.powf(elapsed_hops);
            let frame_peak = self.new_bands.iter().copied().fold(0.0_f32, f32::max);
            for (stored, fresh) in g.values.iter_mut().zip(self.new_bands.iter()) {
                *stored = (*stored * decay).max(*fresh);
            }
            g.peak_envelope = (g.peak_envelope * peak_decay).max(frame_peak);
            g.updated_at = Instant::now();
            drop(g);

            self.sample_buf.drain(..HOP_SIZE);
        }
    }
}

/// Precomputes the `(start, end)` FFT bin ranges for each log-scale band.
///
/// Called once in `Analyzer::new()`; the result is stored and
/// reused every hop so `push()` never runs `powf` per band per frame.
/// Bin 0 (DC component) is skipped by starting `used_up_to` at 1.
fn precompute_band_ranges(num_bins: usize, num_bands: usize) -> Vec<(usize, usize)> {
    let log_min = 1.0_f64;
    let log_max = num_bins as f64;
    let mut used_up_to: usize = 1;
    let mut ranges = Vec::with_capacity(num_bands);
    for band in 0..num_bands {
        if used_up_to >= num_bins {
            // All bins exhausted — pad remaining bands with a silent dummy range.
            ranges.push((num_bins - 1, num_bins));
            continue;
        }
        let t_start = band as f64 / num_bands as f64;
        let t_end = (band + 1) as f64 / num_bands as f64;
        let natural_start = (log_min * (log_max / log_min).powf(t_start)) as usize;
        let natural_end = (log_min * (log_max / log_min).powf(t_end)) as usize;
        // Advance past already-used bins so low-frequency bands do not all
        // share the same FFT bin and produce an identical flat plateau.
        let start = natural_start.max(used_up_to).min(num_bins - 1);
        let end = natural_end.max(start + 1).min(num_bins);
        used_up_to = end;
        ranges.push((start, end));
    }
    ranges
}

/// Fills `out` with the RMS magnitude of each log-scale band using the
/// precomputed bin ranges — no `Vec` allocation and no `powf` per call.
fn fill_log_bands(magnitudes: &[f32], band_ranges: &[(usize, usize)], out: &mut [f32]) {
    for (band_val, &(start, end)) in out.iter_mut().zip(band_ranges.iter()) {
        let len = (end - start) as f32;
        let sum_sq: f32 = magnitudes[start..end].iter().map(|&v| v * v).sum();
        *band_val = (sum_sq / len).sqrt();
    }
}

/// Applies a single pass of 3-point weighted smoothing [0.25, 0.5, 0.25] across
/// adjacent bands to reduce per-bin jitter without blurring transients.
///
/// `scratch` is a caller-supplied buffer (same length as `bands`) used as a
/// temporary copy, avoiding a `Vec` allocation on every hop.
fn smooth_bands(bands: &mut [f32], scratch: &mut [f32]) {
    let n = bands.len();
    if n < 3 {
        return;
    }
    scratch[..n].copy_from_slice(&bands[..n]);
    for i in 0..n {
        let prev = scratch[if i > 0 { i - 1 } else { 0 }];
        let next = scratch[if i + 1 < n { i + 1 } else { n - 1 }];
        bands[i] = prev * 0.25 + scratch[i] * 0.5 + next * 0.25;
    }
}

/// A color in `OKLab`, where straight-line blends change evenly to the eye instead of muddying
/// through gray the way RGB blends do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Oklab {
    l: f32,
    a: f32,
    b: f32,
}

impl Oklab {
    pub fn from_rgb(rgb: [u8; 3]) -> Self {
        let [red, green, blue] = rgb.map(|c| {
            let c = f32::from(c) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        });
        let cone = |x: f32, y: f32, z: f32| (x * red + y * green + z * blue).cbrt();
        let lms = [
            cone(0.412_221_46, 0.536_332_55, 0.051_445_995),
            cone(0.211_903_5, 0.680_699_5, 0.107_396_96),
            cone(0.088_302_46, 0.281_718_85, 0.629_978_7),
        ];
        let mix = |x: f32, y: f32, z: f32| x * lms[0] + y * lms[1] + z * lms[2];
        Self {
            l: mix(0.210_454_26, 0.793_617_8, -0.004_072_047),
            a: mix(1.977_998_5, -2.428_592_2, 0.450_593_7),
            b: mix(0.025_904_037, 0.782_771_77, -0.808_675_77),
        }
    }

    /// From lightness, chroma and hue in degrees, with the chroma reduced until the color fits
    /// in sRGB so its hue survives the conversion.
    fn from_lch(l: f32, c: f32, hue: f32) -> Self {
        let (sin, cos) = hue.to_radians().sin_cos();
        let mut c = c;
        loop {
            let color = Self {
                l,
                a: c * cos,
                b: c * sin,
            };
            if c <= 0.0 || color.linear_rgb().iter().all(|v| (0.0..=1.0).contains(v)) {
                return color;
            }
            c -= 0.005;
        }
    }

    #[cfg(feature = "ascii-cover")]
    fn chroma(self) -> f32 {
        self.a.hypot(self.b)
    }

    #[cfg(feature = "ascii-cover")]
    fn hue(self) -> f32 {
        self.b.atan2(self.a).to_degrees().rem_euclid(360.0)
    }

    fn lerp(self, other: Self, t: f32) -> Self {
        Self {
            l: self.l + (other.l - self.l) * t,
            a: self.a + (other.a - self.a) * t,
            b: self.b + (other.b - self.b) * t,
        }
    }

    /// The same color with its hue turned by an angle given as its sine and cosine.
    fn rotate(self, (sin, cos): (f32, f32)) -> Self {
        Self {
            l: self.l,
            a: self.a * cos - self.b * sin,
            b: self.a * sin + self.b * cos,
        }
    }

    fn linear_rgb(self) -> [f32; 3] {
        let l = (self.l + 0.396_337_78 * self.a + 0.215_803_76 * self.b).powi(3);
        let m = (self.l - 0.105_561_346 * self.a - 0.063_854_17 * self.b).powi(3);
        let s = (self.l - 0.089_484_18 * self.a - 1.291_485_5 * self.b).powi(3);
        [
            4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
            -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
            -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
        ]
    }

    fn color(self) -> Color {
        let [r, g, b] = self.linear_rgb().map(|c| {
            let c = c.clamp(0.0, 1.0);
            let c = if c <= 0.003_130_8 {
                c * 12.92
            } else {
                1.055 * c.powf(1.0 / 2.4) - 0.055
            };
            (c * 255.0).round() as u8
        });
        Color::Rgb(r, g, b)
    }
}

/// How far a blended palette's hue turns from the bass at the left to the treble at the right,
/// in degrees.
const HUE_SWEEP: f32 = 40.0;

/// The bars' gradient from `low` at the base through `mid` to `high` at the top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarPalette {
    low: Oklab,
    mid: Oklab,
    high: Oklab,
}

impl Default for BarPalette {
    /// Dusk: violet at the base, rose through the middle, a warm gold at the top.
    fn default() -> Self {
        Self::from_hues(290.0, 70.0)
    }
}

impl BarPalette {
    /// A gradient rising from a deep shade of `base` to a pale glow of `accent`, through a
    /// vivid hue halfway between them.
    fn from_hues(base: f32, accent: f32) -> Self {
        let mid = base + ((accent - base + 540.0).rem_euclid(360.0) - 180.0) / 2.0;
        Self {
            low: Oklab::from_lch(0.45, 0.13, base),
            mid: Oklab::from_lch(0.68, 0.16, mid),
            high: Oklab::from_lch(0.88, 0.11, accent),
        }
    }

    /// A gradient from a cover's colors: its most prominent hue at the base and its strongest
    /// contrasting hue at the top, or `None` for a cover with too little color to build one.
    #[cfg(feature = "ascii-cover")]
    pub fn from_cover(pixels: impl Iterator<Item = [u8; 3]>) -> Option<Self> {
        const BINS: usize = 24;
        const BIN_DEGREES: f32 = 360.0 / BINS as f32;
        // Per hue bin: the summed weight, and the weighted hue direction
        let mut bins = [(0.0_f32, 0.0_f32, 0.0_f32); BINS];
        let mut count = 0;
        for rgb in pixels {
            count += 1;
            let lab = Oklab::from_rgb(rgb);
            let c = lab.chroma();
            if c < 0.03 {
                continue;
            }
            // Very dark and very light pixels carry little of a cover's character.
            let w = c * (4.0 * lab.l * (1.0 - lab.l)).max(0.0);
            let bin = ((lab.hue() / BIN_DEGREES) as usize).min(BINS - 1);
            bins[bin].0 += w;
            bins[bin].1 += lab.a / c * w;
            bins[bin].2 += lab.b / c * w;
        }
        // Each bin is taken with its neighbours, so a hue split over a bin edge isn't undercounted.
        let around = |i: usize| (0..3).map(move |d| bins[(i + BINS - 1 + d) % BINS]);
        let score = |i: usize| around(i).map(|bin| bin.0).sum::<f32>();
        let hue = |i: usize| {
            let (a, b) = around(i).fold((0.0, 0.0), |(a, b), bin| (a + bin.1, b + bin.2));
            f32::atan2(b, a).to_degrees().rem_euclid(360.0)
        };
        let best = |skip: &dyn Fn(usize) -> bool| {
            (0..BINS)
                .filter(|&i| !skip(i))
                .max_by(|&x, &y| score(x).total_cmp(&score(y)))
        };

        let primary = best(&|_| false)?;
        if score(primary) < 0.01 * count as f32 {
            return None;
        }
        // The accent sits at least 60 degrees round the wheel from the primary hue, and is only
        // taken when it holds a fair share of the cover's color.
        let near = |i: usize| {
            let d = i.abs_diff(primary);
            d.min(BINS - d) < 4
        };
        let base = hue(primary);
        let accent = best(&near)
            .filter(|&i| score(i) >= 0.2 * score(primary))
            .map_or(base + HUE_SWEEP, hue);
        Some(Self::from_hues(base, accent))
    }

    /// The color at height `t` in [0, 1], its hue turned by `turn` (sine, cosine).
    fn at(&self, t: f32, turn: (f32, f32)) -> Color {
        let lab = if t < 0.5 {
            self.low.lerp(self.mid, t * 2.0)
        } else {
            self.mid.lerp(self.high, (t - 0.5) * 2.0)
        };
        lab.rotate(turn).color()
    }
}

/// The bars' colors: the theme's, or a cover-drawn or default gradient when the theme leaves
/// them unset.
enum BarColors {
    /// Theme colors that include ANSI palette entries, whose actual values aren't known, so each
    /// cell takes the nearest stop instead of a blend.
    Stops(config::VisualizationColors),
    Blend(BarPalette),
}

impl BarColors {
    fn new(theme: &config::Theme, cover: Option<BarPalette>) -> Self {
        let Some(colors) = theme.visualization() else {
            return Self::Blend(cover.unwrap_or_default());
        };
        match (colors.low, colors.mid, colors.high) {
            (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2), Color::Rgb(r3, g3, b3)) => {
                Self::Blend(BarPalette {
                    low: Oklab::from_rgb([r1, g1, b1]),
                    mid: Oklab::from_rgb([r2, g2, b2]),
                    high: Oklab::from_rgb([r3, g3, b3]),
                })
            }
            _ => Self::Stops(colors),
        }
    }

    /// The color at height `t` in [0, 1]; a blend's hue is turned by `turn` (sine, cosine).
    fn at(&self, t: f32, turn: (f32, f32)) -> Color {
        match self {
            Self::Stops(c) => match t {
                t if t < 0.25 => c.low,
                t if t < 0.75 => c.mid,
                _ => c.high,
            },
            Self::Blend(palette) => palette.at(t, turn),
        }
    }

    /// The color that marks the track around the bars: its title, play state and progress.
    fn accent(&self) -> Color {
        self.at(0.85, NO_TURN)
    }
}

/// A hue turn of zero, as its sine and cosine.
const NO_TURN: (f32, f32) = (0.0, 1.0);

/// The current per-band levels in [0, 1] on a perceptual scale, or `None` when nothing is
/// streaming locally.
pub fn band_levels(state: &SharedState) -> Option<[f32; NUM_BANDS]> {
    // display_decay interpolates levels smoothly between push() calls.
    // We normalise against peak_envelope (NOT the per-frame peak), so display_decay
    // no longer cancels out and levels genuinely fade between audio packets.
    let guard = state.vis_bands.as_ref()?.lock();
    if !guard.is_active {
        return None;
    }
    let elapsed = guard.updated_at.elapsed();
    let display_decay = decay_for_elapsed(elapsed);
    let peak_norm = (guard.peak_envelope * peak_decay_for_elapsed(elapsed)).max(1e-6);
    // Copy the fixed-size array by value — no heap allocation.
    let mut levels = guard.values;
    drop(guard);
    for level in &mut levels {
        // Sqrt (gamma 0.5) scaling boosts quiet signals without clipping louds.
        *level = ((*level * display_decay) / peak_norm)
            .clamp(0.0, 1.0)
            .powf(0.5);
    }
    Some(levels)
}

/// Render the visualization in place of the page and the playback window: the ASCII cover (with
/// the `ascii-cover` feature) at the left, the bars filling the rest, and a transport line
/// beneath them.
pub fn render_visualization_page(
    frame: &mut Frame,
    state: &SharedState,
    ui: &mut UIStateGuard,
    rect: Rect,
) {
    use ratatui::layout::{Constraint, Layout};

    // A blank line separates the transport line from the panel.
    let [panel, _, transport] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(rect);
    let quiet = ratatui::style::Style::default().fg(Color::DarkGray);
    let rect = construct_and_render_styled_block(
        " Visualization ",
        quiet,
        quiet,
        Borders::ALL,
        frame,
        panel,
    );
    #[cfg(feature = "ascii-cover")]
    let (rect, colors) = render_ascii_cover(frame, state, ui, rect);
    #[cfg(not(feature = "ascii-cover"))]
    let colors = BarColors::new(&ui.theme, None);
    render_audio_visualization(frame, state, &colors, rect);
    render_transport(frame, state, ui, &colors, transport);
}

/// Render the play state, elapsed time, a progress line colored like the bars, the duration,
/// and then the volume, device and what plays next, across `rect`.
fn render_transport(
    frame: &mut Frame,
    state: &SharedState,
    ui: &mut UIStateGuard,
    colors: &BarColors,
    rect: Rect,
) {
    use crate::utils::format_duration;
    use ratatui::{
        layout::{Constraint, Layout},
        style::{Modifier, Style},
        text::{Line, Span},
    };
    use rspotify::model::{PlayableItem, RepeatState};

    let player = state.player.read();
    let Some(playback) = player.buffered_playback.as_ref() else {
        return;
    };
    let duration = match player.currently_playing() {
        Some(PlayableItem::Track(track)) => track.duration,
        Some(PlayableItem::Episode(episode)) => episode.duration,
        Some(PlayableItem::Unknown(_)) | None => return,
    };
    let progress = player
        .playback_progress()
        .unwrap_or_default()
        .clamp(chrono::Duration::zero(), duration);
    let configs = config::get_config();
    let accent = colors.accent();
    let quiet = Style::default().fg(Color::DarkGray);

    let icon = if playback.is_playing {
        &configs.app_config.play_icon
    } else {
        &configs.app_config.pause_icon
    };
    let left = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            icon.clone(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  {} ", format_duration(&progress))),
    ]);

    // Repeat and shuffle only show while they're on.
    let mut fields = Vec::new();
    for field in &configs.app_config.playback_metadata_fields {
        match field.as_str() {
            "volume" => fields.push((
                "vol",
                match playback.mute_state {
                    Some(volume) => format!("{volume}% muted"),
                    None => format!("{}%", playback.volume.unwrap_or_default()),
                },
            )),
            "device" => fields.push(("on", playback.device_name.clone())),
            "repeat" if playback.repeat_state != RepeatState::Off => fields.push((
                "repeat",
                <&'static str>::from(playback.repeat_state).to_string(),
            )),
            "shuffle" if playback.shuffle_state => fields.push(("shuffle", "on".to_string())),
            _ => {}
        }
    }
    fields.push((
        "next",
        state
            .session
            .lock()
            .session
            .continue_mode
            .label()
            .to_string(),
    ));
    if !ui.mouse_capture {
        fields.push(("mouse", "off (select text)".to_string()));
    }
    if !crate::cast::is_listening() {
        fields.push(("here", "muted".to_string()));
    }
    let mut right = vec![Span::styled(
        format!(" {}", format_duration(&duration)),
        quiet,
    )];
    for (label, value) in fields {
        right.push(Span::styled(format!("   {label} "), quiet));
        right.push(Span::raw(value));
    }
    let right = Line::from(right);

    let [left_rect, bar, right_rect] = Layout::horizontal([
        Constraint::Length(left.width() as u16),
        Constraint::Fill(1),
        Constraint::Length(right.width() as u16),
    ])
    .areas(rect);
    frame.render_widget(left, left_rect);
    frame.render_widget(right, right_rect);

    let width = usize::from(bar.width);
    let filled = (progress.num_milliseconds() as f32 / duration.num_milliseconds().max(1) as f32
        * width as f32) as usize;
    let buf = frame.buffer_mut();
    for i in 0..width {
        let (ch, fg) = match i.cmp(&filled) {
            std::cmp::Ordering::Less => (
                '━',
                colors.at(0.35 + 0.6 * i as f32 / width as f32, NO_TURN),
            ),
            std::cmp::Ordering::Equal => ('●', accent),
            std::cmp::Ordering::Greater => ('─', Color::DarkGray),
        };
        buf[(bar.x + i as u16, bar.y)].set_char(ch).set_fg(fg);
    }
    // clicking the line seeks, as on the playback window's progress bar
    ui.playback_progress_bar_rect = bar;
}

/// Render the ASCII cover at the left of `rect`, as large as fits in up to half its width with
/// the track, artists and album captioned beneath it, and return the area left for the bars
/// with the bars' colors, drawn from the cover's unless the theme sets them.
#[cfg(feature = "ascii-cover")]
fn render_ascii_cover(
    frame: &mut Frame,
    state: &SharedState,
    ui: &mut UIStateGuard,
    rect: Rect,
) -> (Rect, BarColors) {
    use ratatui::{
        layout::{Alignment, Constraint, Flex, Layout},
        style::{Modifier, Style},
        text::Line,
        widgets::Paragraph,
    };

    let no_cover = |ui: &UIStateGuard| (rect, BarColors::new(&ui.theme, None));
    if ui.ascii_cover.mode == config::AsciiCoverMode::Off {
        return no_cover(ui);
    }
    let player = state.player.read();
    let Some(item) = player
        .playback
        .as_ref()
        .and_then(|playback| playback.item.as_ref())
    else {
        return no_cover(ui);
    };
    let Some(url) = super::playback::item_image_url(item) else {
        return no_cover(ui);
    };
    let data = state.data.read();
    let Some(img) = data.caches.images.get(&url) else {
        return no_cover(ui);
    };
    let Some([name, artists, album]) = cover_caption(item) else {
        return no_cover(ui);
    };
    // A blank line separates the caption from the cover.
    let caption_height = 4;
    // A cell is about twice as tall as it is wide, so a square cover is twice as wide as tall.
    let width = (rect.height.saturating_sub(caption_height) * 2).min(rect.width / 2);
    let height = width / 2;
    let [cover_column, _, rest] = Layout::horizontal([
        Constraint::Length(width),
        Constraint::Length(1),
        Constraint::Fill(0),
    ])
    .areas(rect);
    let [cover_rect, caption_rect] = Layout::vertical([
        Constraint::Length(height),
        Constraint::Length(caption_height),
    ])
    .flex(Flex::Center)
    .areas(cover_column);
    let levels = band_levels(state);
    // The cover is rendered first, since that draws the palette the caption is colored from.
    ui.ascii_cover.render(
        frame,
        &url,
        img,
        cover_rect,
        levels.as_ref().map(|l| &l[..]),
    );
    let colors = BarColors::new(&ui.theme, ui.ascii_cover.palette());
    let caption = vec![
        Line::default(),
        Line::styled(
            name,
            Style::default()
                .fg(colors.accent())
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(artists),
        Line::styled(
            album,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    ];
    frame.render_widget(
        Paragraph::new(caption).alignment(Alignment::Center),
        caption_rect,
    );
    (rest, colors)
}

/// The track (or episode), its artists (or publisher) and its album (or show).
#[cfg(feature = "ascii-cover")]
fn cover_caption(item: &rspotify::model::PlayableItem) -> Option<[String; 3]> {
    use crate::utils::to_bidi_string;
    use rspotify::model::PlayableItem;

    #[allow(deprecated)]
    let (name, artists, album) = match item {
        PlayableItem::Track(track) => (
            &track.name,
            crate::utils::map_join(&track.artists, |a| &a.name, ", "),
            &track.album.name,
        ),
        PlayableItem::Episode(episode) => (
            &episode.name,
            episode.show.publisher.clone(),
            &episode.show.name,
        ),
        PlayableItem::Unknown(_) => return None,
    };
    Some([
        to_bidi_string(name),
        to_bidi_string(&artists),
        to_bidi_string(album),
    ])
}

/// Each band's recent peak: raised to `levels`, sinking at `PEAK_FALL_PER_SEC` since the last
/// call.
fn track_peaks(state: &SharedState, levels: &[f32; NUM_BANDS]) -> [f32; NUM_BANDS] {
    let Some(bands) = state.vis_bands.as_ref() else {
        return *levels;
    };
    let mut g = bands.lock();
    let fall = g.peaks_at.elapsed().as_secs_f32() * PEAK_FALL_PER_SEC;
    g.peaks_at = Instant::now();
    for (peak, &level) in g.peaks.iter_mut().zip(levels) {
        *peak = (*peak - fall).max(level);
    }
    g.peaks
}

/// The top cell of a bar `h` rows tall between neighbours `left` and `right` rows tall: a peak,
/// a slope, or a flat top whose glyph sits as high in the cell as the bar reaches into it.
fn cap_glyph(left: f32, h: f32, right: f32) -> char {
    if left < h - 0.5 && right < h - 0.5 {
        '^'
    } else if right > h + 0.5 {
        '/'
    } else if left > h + 0.5 {
        '\\'
    } else {
        match h.fract() {
            f if f < 1.0 / 3.0 => '_',
            f if f < 2.0 / 3.0 => '-',
            _ => '~',
        }
    }
}

/// Render the frequency bands as ASCII bars filling `rect`, bass at the left.
///
/// Each bar's body thins from `@` at the base toward `.`, its top is shaped by its neighbours,
/// and a `'` marks its recent peak. Cells are colored by height, `low` at the base → `mid` →
/// `high` at the top, and a blended palette's hue turns gradually from bass to treble.
fn render_audio_visualization(
    frame: &mut Frame,
    state: &SharedState,
    colors: &BarColors,
    rect: Rect,
) {
    let Some(levels) = band_levels(state) else {
        return;
    };
    if rect.is_empty() {
        return;
    }
    let peaks = track_peaks(state, &levels);
    let rows = f32::from(rect.height);
    let width = usize::from(rect.width);
    // Columns share bands when the area is wider than `NUM_BANDS` and skip some when narrower.
    let band = |col: usize| col * NUM_BANDS / width;
    let heights: Vec<f32> = (0..width).map(|col| levels[band(col)] * rows).collect();
    // The row `r` cells up from the bottom
    let y = |r: u16| rect.bottom() - 1 - r;

    let buf = frame.buffer_mut();
    for (col, &h) in heights.iter().enumerate() {
        let x = rect.x + col as u16;
        let turn = (HUE_SWEEP * (col as f32 / width as f32 - 0.5))
            .to_radians()
            .sin_cos();
        let color = |t: f32| colors.at(t, turn);
        let top = h as u16;
        for r in 0..top.min(rect.height) {
            let t = f32::from(r) / rows;
            let ch = BAR_BODY[(t * BAR_BODY.len() as f32) as usize];
            buf[(x, y(r))].set_char(char::from(ch)).set_fg(color(t));
        }
        if top < rect.height {
            let left = if col > 0 { heights[col - 1] } else { h };
            let right = heights.get(col + 1).copied().unwrap_or(h);
            buf[(x, y(top))]
                .set_char(cap_glyph(left, h, right))
                .set_fg(color(h / rows));
        }
        let peak = peaks[band(col)];
        let peak_row = (peak * rows) as u16;
        if peak_row > top + 1 && peak_row < rect.height {
            buf[(x, y(peak_row))].set_char('\'').set_fg(color(peak));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oklab_round_trips_rgb() {
        for rgb in [[0, 0, 0], [255, 255, 255], [200, 30, 90], [12, 140, 250]] {
            assert_eq!(
                Oklab::from_rgb(rgb).color(),
                Color::Rgb(rgb[0], rgb[1], rgb[2])
            );
        }
    }

    #[cfg(feature = "ascii-cover")]
    #[test]
    fn cover_palette_follows_the_cover_hues() {
        let hue_of = |rgb: [u8; 3]| Oklab::from_rgb(rgb).hue();
        let angle = |a: f32, b: f32| {
            let d = (a - b).rem_euclid(360.0);
            d.min(360.0 - d)
        };
        // mostly red with a blue accent: red at the base, blue at the top
        let cover =
            std::iter::repeat_n([200, 40, 40], 70).chain(std::iter::repeat_n([40, 60, 200], 30));
        let palette = BarPalette::from_cover(cover).unwrap();
        assert!(angle(palette.low.hue(), hue_of([200, 40, 40])) < 15.0);
        assert!(angle(palette.high.hue(), hue_of([40, 60, 200])) < 15.0);
        // a gray cover has no colors to draw from
        assert_eq!(
            BarPalette::from_cover(std::iter::repeat_n([128; 3], 100)),
            None
        );
    }

    #[test]
    fn cap_glyph_follows_the_neighbours() {
        assert_eq!(cap_glyph(1.0, 3.0, 1.0), '^');
        assert_eq!(cap_glyph(3.0, 3.0, 5.0), '/');
        assert_eq!(cap_glyph(5.0, 3.0, 3.0), '\\');
        assert_eq!(cap_glyph(3.1, 3.1, 3.1), '_');
        assert_eq!(cap_glyph(3.5, 3.5, 3.5), '-');
        assert_eq!(cap_glyph(3.9, 3.9, 3.9), '~');
    }
}
