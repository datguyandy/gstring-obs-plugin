//! Real-time PCM downmixer/resampler and RMS VAD gate.
//!
//! Mirrors `web/lib/overlay/capture.ts` so the OBS audio filter produces the
//! exact same 16 kHz mono Int16 PCM 100 ms chunks and silence-gating behaviour
//! as the browser capture page.

use std::collections::VecDeque;

use bytes::Bytes;

/// Sample rate advertised in `hello` and sent to Soniox.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// 100 ms at 16 kHz (3,200 bytes of little-endian Int16 PCM).
pub const CHUNK_SAMPLES: usize = 1_600;

/// Default RMS threshold matching `web/lib/overlay/capture.ts`.
pub const DEFAULT_NOISE_FLOOR: f32 = 0.008;

/// Frames buffered before gate opens (3 × 100 ms = 300 ms) so the opening
/// syllable of an utterance is never clipped.
const PREROLL_FRAMES: usize = 3;

/// Frames kept flowing after RMS drops below the floor (8 × 100 ms = 800 ms)
/// so quiet trailing consonants are not clipped.
const HANGOVER_FRAMES: usize = 8;

/// One 100 ms little-endian Int16 mono PCM chunk plus its RMS loudness.
#[derive(Debug, Clone)]
pub struct PcmChunk {
    pub pcm: Bytes,
    pub rms: f32,
}

/// Downmixes planar `f32` OBS audio buffers to mono, linearly resamples to
/// 16 kHz, and emits 100 ms Int16 PCM chunks.
pub struct PcmResampler {
    target_rate: u32,
    chunk_samples: usize,
    out: Vec<f32>,
    carry: Vec<f32>,
    merged: Vec<f32>,
    read_frac: f64,
}

impl Default for PcmResampler {
    fn default() -> Self {
        Self::new(TARGET_SAMPLE_RATE, CHUNK_SAMPLES)
    }
}

impl PcmResampler {
    #[must_use]
    pub fn new(target_rate: u32, chunk_samples: usize) -> Self {
        Self {
            target_rate: target_rate.max(8_000),
            chunk_samples: chunk_samples.max(160),
            out: Vec::with_capacity(chunk_samples.max(160)),
            carry: Vec::with_capacity(64),
            merged: Vec::with_capacity(2048),
            read_frac: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.out.clear();
        self.carry.clear();
        self.merged.clear();
        self.read_frac = 0.0;
    }

    /// Push one planar multi-channel `f32` block at `input_rate` Hz and invoke
    /// `emit` for each complete 100 ms chunk produced.
    pub fn push_planar(
        &mut self,
        planes: &[&[f32]],
        input_rate: u32,
        mut emit: impl FnMut(PcmChunk),
    ) {
        if planes.is_empty() || input_rate == 0 {
            return;
        }
        let frames = planes.iter().map(|p| p.len()).min().unwrap_or(0);
        if frames == 0 {
            return;
        }

        // Reuse `merged` buffer to avoid per-callback heap allocation on the
        // real-time OBS audio thread.
        self.merged.clear();
        self.merged.extend_from_slice(&self.carry);

        if planes.len() == 1 {
            self.merged.extend_from_slice(&planes[0][..frames]);
        } else {
            let inv_channels = 1.0 / (planes.len() as f32);
            let start = self.merged.len();
            self.merged.resize(start + frames, 0.0);
            for plane in planes {
                for (dst, &sample) in self.merged[start..].iter_mut().zip(&plane[..frames]) {
                    *dst += sample;
                }
            }
            for sample in &mut self.merged[start..] {
                *sample *= inv_channels;
            }
        }

        let ratio = f64::from(input_rate) / f64::from(self.target_rate);
        let mut read = self.read_frac;
        let len = self.merged.len();

        while read + 1.0 < len as f64 {
            let idx = read as usize;
            let frac = (read - idx as f64) as f32;
            let sample = self.merged[idx] * (1.0 - frac) + self.merged[idx + 1] * frac;
            self.out.push(sample);

            if self.out.len() == self.chunk_samples {
                let mut bytes = Vec::with_capacity(self.chunk_samples * 2);
                let mut sum_sq = 0.0f64;
                for &raw in &self.out {
                    let s = if raw.is_finite() {
                        raw.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    };
                    let i16_val = if s < 0.0 {
                        (s * 32768.0).clamp(-32768.0, 32767.0) as i16
                    } else {
                        (s * 32767.0).clamp(-32768.0, 32767.0) as i16
                    };
                    bytes.extend_from_slice(&i16_val.to_le_bytes());
                    sum_sq += f64::from(s) * f64::from(s);
                }
                let rms = (sum_sq / (self.chunk_samples as f64)).sqrt() as f32;
                self.out.clear();
                emit(PcmChunk {
                    pcm: Bytes::from(bytes),
                    rms,
                });
            }

            read += ratio;
        }

        let consumed = (read.floor() as usize).min(len);
        self.carry.clear();
        self.carry.extend_from_slice(&self.merged[consumed..]);
        self.read_frac = (read - consumed as f64).max(0.0);
    }
}

/// RMS voice-activity gate matching `VadGate` in `web/lib/overlay/capture.ts`.
pub struct VadGate {
    noise_floor: f32,
    open: bool,
    quiet_frames: usize,
    preroll: VecDeque<Bytes>,
}

impl Default for VadGate {
    fn default() -> Self {
        Self::new(DEFAULT_NOISE_FLOOR)
    }
}

impl VadGate {
    #[must_use]
    pub fn new(noise_floor: f32) -> Self {
        Self {
            noise_floor,
            open: false,
            quiet_frames: 0,
            preroll: VecDeque::with_capacity(PREROLL_FRAMES + 1),
        }
    }

    pub fn set_noise_floor(&mut self, noise_floor: f32) {
        self.noise_floor = noise_floor.clamp(0.0005, 0.2);
    }

    pub fn reset(&mut self) {
        self.open = false;
        self.quiet_frames = 0;
        self.preroll.clear();
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Feed one 100 ms chunk and emit zero, one, or pre-roll+1 chunks to send.
    pub fn push(&mut self, chunk: PcmChunk, mut emit: impl FnMut(Bytes)) {
        let loud = chunk.rms >= self.noise_floor;

        if self.open {
            if loud {
                self.quiet_frames = 0;
            } else {
                self.quiet_frames += 1;
                if self.quiet_frames > HANGOVER_FRAMES {
                    self.open = false;
                    self.preroll.clear();
                    self.preroll.push_back(chunk.pcm);
                    return;
                }
            }
            emit(chunk.pcm);
            return;
        }

        if loud {
            self.open = true;
            self.quiet_frames = 0;
            while let Some(frame) = self.preroll.pop_front() {
                emit(frame);
            }
            emit(chunk.pcm);
            return;
        }

        self.preroll.push_back(chunk.pcm);
        while self.preroll.len() > PREROLL_FRAMES {
            self.preroll.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_downmixes_48khz_stereo_to_16khz_mono_100ms_chunks() {
        let mut resampler = PcmResampler::default();
        // 4,800 samples at 48 kHz = 100 ms; add a tiny extra tail so linear
        // interpolation reaches sample index 4,799 cleanly.
        let left = vec![0.5f32; 4_810];
        let right = vec![0.0f32; 4_810];
        let mut chunks = Vec::new();

        resampler.push_planar(&[&left, &right], 48_000, |chunk| chunks.push(chunk));

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].pcm.len(), CHUNK_SAMPLES * 2);
        // Mono average of 0.5 and 0.0 is 0.25.
        assert!((chunks[0].rms - 0.25).abs() < 0.01);

        let first = i16::from_le_bytes([chunks[0].pcm[0], chunks[0].pcm[1]]);
        assert!((first - 8_191).abs() <= 2);
    }

    #[test]
    fn resampler_handles_44100hz_across_small_callbacks() {
        let mut resampler = PcmResampler::default();
        let mut chunks = Vec::new();
        // Feed 1 second of 44.1 kHz audio in 256-sample OBS-style blocks.
        let block = vec![0.2f32; 256];
        for _ in 0..173 {
            resampler.push_planar(&[&block], 44_100, |c| chunks.push(c));
        }
        // ~1.004 seconds should yield 10 complete 100 ms chunks.
        assert_eq!(chunks.len(), 10);
        for chunk in &chunks {
            assert_eq!(chunk.pcm.len(), CHUNK_SAMPLES * 2);
            assert!((chunk.rms - 0.2).abs() < 0.01);
        }
    }

    #[test]
    fn vad_gate_flushes_preroll_and_holds_hangover() {
        let mut gate = VadGate::new(0.01);
        let mk = |tag: u8, rms: f32| PcmChunk {
            pcm: Bytes::from(vec![tag; 4]),
            rms,
        };

        let mut sent = Vec::new();
        // 4 quiet frames -> none sent, last 3 kept in pre-roll.
        for tag in 1..=4 {
            gate.push(mk(tag, 0.001), |b| sent.push(b[0]));
        }
        assert!(sent.is_empty());
        assert!(!gate.is_open());

        // Loud frame -> flushes tags 2, 3, 4 then 5.
        gate.push(mk(5, 0.05), |b| sent.push(b[0]));
        assert_eq!(sent, vec![2, 3, 4, 5]);
        assert!(gate.is_open());

        // 8 quiet frames stay open (hangover), 9th closes.
        sent.clear();
        for i in 0..8 {
            gate.push(mk(10 + i, 0.001), |b| sent.push(b[0]));
        }
        assert_eq!(sent.len(), 8);
        assert!(gate.is_open());

        sent.clear();
        gate.push(mk(99, 0.001), |b| sent.push(b[0]));
        assert!(sent.is_empty());
        assert!(!gate.is_open());
    }
}
