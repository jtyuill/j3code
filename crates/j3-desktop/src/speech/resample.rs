//! Converts microphone audio to the 16 kHz mono Whisper hears: band-limited
//! (windowed-sinc) interpolation from a precomputed polyphase table, fed
//! incrementally as the recording grows.

use std::f64::consts::PI;

/// Whisper's sample rate.
pub const WHISPER_RATE: u32 = 16_000;

/// Zero crossings of the sinc kernel on each side, at the output rate.
const ZERO_CROSSINGS: f64 = 16.;
/// Fractional positions between input samples the kernel is tabulated at.
const PHASES: usize = 512;

pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Kernel taps on each side of the interpolation point.
    half: usize,
    /// `PHASES + 1` rows of `2 * half` taps; row `p` is the kernel for an
    /// interpolation point `p / PHASES` past an input sample.
    table: Vec<f32>,
    /// Every input sample so far.
    input: Vec<f32>,
    /// Output samples produced so far.
    produced: usize,
}

impl Resampler {
    /// A resampler from `rate` to [`WHISPER_RATE`].
    pub fn new(rate: u32) -> Self {
        let step = f64::from(rate) / f64::from(WHISPER_RATE);
        // Downsampling moves the cutoff below the output's Nyquist rate, a
        // little under it so the transition band does not alias.
        let cutoff = (1. / step).min(1.) * 0.92;
        let half = (ZERO_CROSSINGS / cutoff).ceil() as usize;
        let mut table = Vec::with_capacity((PHASES + 1) * 2 * half);
        for phase in 0..=PHASES {
            let frac = phase as f64 / PHASES as f64;
            let row: Vec<f64> = (0..2 * half)
                .map(|tap| {
                    // Distance from the interpolation point to the input
                    // sample this tap weighs, in input samples.
                    let x = frac + half as f64 - 1. - tap as f64;
                    cutoff * sinc(cutoff * x) * blackman(x / half as f64)
                })
                .collect();
            let gain: f64 = row.iter().sum();
            table.extend(row.into_iter().map(|w| (w / gain) as f32));
        }
        Self {
            step,
            half,
            table,
            input: Vec::new(),
            produced: 0,
        }
    }

    /// Takes the next input samples and appends to `out` every output sample
    /// whose kernel they complete. `last` treats the input as ended, so the
    /// tail is produced too (missing samples count as silence).
    pub fn push(&mut self, input: &[f32], last: bool, out: &mut Vec<f32>) {
        self.input.extend_from_slice(input);
        let available = self.input.len();
        loop {
            let t = self.produced as f64 * self.step;
            let base = t.floor() as usize;
            if base >= available || (!last && base + self.half >= available) {
                break;
            }
            let phase = ((t - base as f64) * PHASES as f64).round() as usize;
            let row = &self.table[phase * 2 * self.half..(phase + 1) * 2 * self.half];
            // Tap `j` weighs input sample `base + 1 - half + j`.
            let first = base as isize + 1 - self.half as isize;
            let mut acc = 0f32;
            for (j, weight) in row.iter().enumerate() {
                let k = first + j as isize;
                if k >= 0
                    && let Some(sample) = self.input.get(k as usize)
                {
                    acc += weight * sample;
                }
            }
            out.push(acc);
            self.produced += 1;
        }
    }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// The Blackman window over `r` in `-1..=1`, zero outside it.
fn blackman(r: f64) -> f64 {
    if r.abs() >= 1. {
        return 0.;
    }
    0.42 + 0.5 * (PI * r).cos() + 0.08 * (2. * PI * r).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f64, seconds: f64) -> Vec<f32> {
        (0..(f64::from(rate) * seconds) as usize)
            .map(|n| (2. * PI * hz * n as f64 / f64::from(rate)).sin() as f32 * 0.5)
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn resample(rate: u32, input: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        Resampler::new(rate).push(input, true, &mut out);
        out
    }

    #[test]
    fn speech_band_tones_keep_their_pitch_and_level() {
        for rate in [44_100, 48_000, 16_000, 8_000] {
            let out = resample(rate, &tone(rate, 440., 1.));
            assert!(out.len().abs_diff(16_000) <= 1, "{rate}: {}", out.len());
            // Away from the edges, a 440 Hz sine at 16 kHz.
            let middle = &out[2_000..14_000];
            let expected = tone(WHISPER_RATE, 440., 1.);
            let level = rms(middle) / rms(&expected[2_000..14_000]);
            assert!((level - 1.).abs() < 0.02, "{rate}: level {level}");
            let crossings = middle
                .windows(2)
                .filter(|pair| pair[0] < 0. && pair[1] >= 0.)
                .count();
            // 12000 samples of 440 Hz at 16 kHz: 330 cycles.
            assert!(crossings.abs_diff(330) <= 1, "{rate}: {crossings}");
        }
    }

    #[test]
    fn tones_above_the_new_nyquist_rate_do_not_fold_back() {
        // 12 kHz at 48 kHz would alias to 4 kHz at 16 kHz.
        let out = resample(48_000, &tone(48_000, 12_000., 1.));
        assert!(rms(&out[2_000..14_000]) < 0.005);
    }

    #[test]
    fn feeding_chunks_matches_feeding_everything_at_once() {
        let input = tone(44_100, 1_000., 0.5);
        let whole = resample(44_100, &input);
        let mut resampler = Resampler::new(44_100);
        let mut chunked = Vec::new();
        for chunk in input.chunks(441) {
            resampler.push(chunk, false, &mut chunked);
        }
        resampler.push(&[], true, &mut chunked);
        assert_eq!(chunked, whole);
    }
}
