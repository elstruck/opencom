//! Speech-to-text: recognizers, voice activity detection, and the per-channel pipeline.
//!
//! Design: every channel runs its own VAD on a dedicated thread. When the VAD sees speech,
//! the pipeline decodes the growing utterance every `partial_interval` seconds and shows it as
//! an in-progress entry. When the utterance ends, it decodes the whole segment once more and
//! finalizes the entry. All channels share one loaded model.

pub mod catalog;
pub mod models;
mod pipeline;
#[cfg(feature = "stt")]
mod sherpa;

pub use pipeline::{AudioMsg, ChannelPipeline, FixedRecognizer, PipelineConfig, StreamingPipeline};

use anyhow::Result;
use std::sync::Arc;

pub const SAMPLE_RATE: u32 = 16_000;

/// Limits how many decodes run at once across all channels.
/// Final decodes wait for a slot; partial decodes skip when no slot is free,
/// so a busy machine drops in-progress text instead of falling behind.
pub struct DecodeGate {
    max: usize,
    used: parking_lot::Mutex<usize>,
    cv: parking_lot::Condvar,
}

pub struct GateSlot<'a>(&'a DecodeGate);

impl Drop for GateSlot<'_> {
    fn drop(&mut self) {
        *self.0.used.lock() -= 1;
        self.0.cv.notify_one();
    }
}

impl DecodeGate {
    pub fn new(max: usize) -> Arc<Self> {
        Arc::new(Self { max: max.max(1), used: parking_lot::Mutex::new(0), cv: parking_lot::Condvar::new() })
    }

    /// A gate sized for this machine: about half the cores, divided by threads per decode.
    pub fn for_machine(threads_per_decode: i32) -> Arc<Self> {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        Self::new((cores / 2 / threads_per_decode.max(1) as usize).max(1))
    }

    pub fn acquire(&self) -> GateSlot<'_> {
        let mut u = self.used.lock();
        while *u >= self.max {
            self.cv.wait(&mut u);
        }
        *u += 1;
        GateSlot(self)
    }

    pub fn try_acquire(&self) -> Option<GateSlot<'_>> {
        let mut u = self.used.lock();
        if *u >= self.max {
            return None;
        }
        *u += 1;
        Some(GateSlot(self))
    }
}

/// Turns a finished chunk of 16 kHz mono audio into text.
pub trait Recognizer: Send + Sync {
    fn transcribe(&self, samples: &[f32]) -> Result<String>;
    fn name(&self) -> String;
}

/// Finds speech in a stream of 16 kHz mono audio.
pub trait Segmenter: Send {
    /// Feed audio. Returns finished speech segments.
    fn accept(&mut self, samples: &[f32]) -> Vec<Vec<f32>>;
    /// True while speech is going on.
    fn speaking(&self) -> bool;
    /// Return any speech that is still buffered (end of stream).
    fn flush(&mut self) -> Vec<Vec<f32>>;
}

pub type SharedRecognizer = Arc<dyn Recognizer>;

/// One channel's live decoding state in a streaming model.
pub trait StreamSession: Send {
    fn accept(&mut self, samples: &[f32]);
    /// Current text of the utterance in progress.
    fn text(&self) -> String;
    /// The model detected the end of an utterance.
    fn is_endpoint(&self) -> bool;
    /// Start a new utterance.
    fn reset(&mut self);
}

/// A streaming model; creates one session per channel.
pub trait StreamingRecognizer: Send + Sync {
    fn session(&self) -> Box<dyn StreamSession>;
    fn name(&self) -> String;
}

/// A loaded model of either kind.
#[derive(Clone)]
pub enum Engine {
    Offline(SharedRecognizer),
    Streaming(Arc<dyn StreamingRecognizer>),
}

impl Engine {
    pub fn name(&self) -> String {
        match self {
            Engine::Offline(r) => r.name(),
            Engine::Streaming(r) => r.name(),
        }
    }
}

/// Streaming models print UPPER CASE without punctuation; make it readable.
pub fn sentence_case(t: &str) -> String {
    let t = t.trim();
    if t.chars().any(|c| c.is_lowercase()) {
        return t.to_string();
    }
    let lower = t.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut first = true;
    for w in lower.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        if w == "i" || w.starts_with("i'") {
            out.push('I');
            out.push_str(&w[1..]);
        } else if first {
            let mut c = w.chars();
            if let Some(f) = c.next() {
                out.extend(f.to_uppercase());
                out.push_str(c.as_str());
            }
        } else {
            out.push_str(w);
        }
        first = false;
    }
    out
}

/// Build a segmenter factory: Silero VAD when available, energy VAD otherwise.
pub fn segmenter_factory(vad_model: Option<std::path::PathBuf>, cfg: &crate::model::SttSettings) -> Arc<dyn Fn() -> Box<dyn Segmenter> + Send + Sync> {
    let threshold = cfg.vad_threshold;
    let min_silence = cfg.min_silence;
    let max_speech = cfg.max_speech;
    #[cfg(feature = "stt")]
    if let Some(p) = vad_model.filter(|p| p.exists()) {
        let path = p.to_string_lossy().to_string();
        return Arc::new(move || match sherpa::SileroSegmenter::new(&path, threshold, min_silence, max_speech) {
            Ok(s) => Box::new(s) as Box<dyn Segmenter>,
            Err(e) => {
                tracing::warn!("Silero VAD failed ({e:#}); using energy VAD");
                Box::new(EnergySegmenter::new(min_silence, max_speech))
            }
        });
    }
    #[cfg(not(feature = "stt"))]
    let _ = (vad_model, threshold);
    Arc::new(move || Box::new(EnergySegmenter::new(min_silence, max_speech)) as Box<dyn Segmenter>)
}

/// Load a catalog model.
pub fn load_engine(cfg: &crate::model::SttSettings) -> Result<Engine> {
    #[cfg(feature = "stt")]
    {
        let spec = catalog::find(&cfg.model).ok_or_else(|| anyhow::anyhow!("unknown model {:?} (see `opencom models`)", cfg.model))?;
        let dir = models::models_dir(cfg).join(spec.dir);
        if !models::is_installed(&models::models_dir(cfg), spec) {
            anyhow::bail!("model {} is not downloaded; run `opencom models download {}`", spec.id, spec.id);
        }
        if spec.kind == catalog::ModelKind::StreamingTransducer {
            return Ok(Engine::Streaming(Arc::new(sherpa::SherpaStreaming::load(spec, &dir, cfg)?)));
        }
        Ok(Engine::Offline(Arc::new(sherpa::SherpaRecognizer::load(spec, &dir, cfg.threads)?)))
    }
    #[cfg(not(feature = "stt"))]
    {
        let _ = cfg;
        anyhow::bail!("this build has no speech-to-text (built without the `stt` feature)")
    }
}

/// Load an offline (VAD-segmented) recognizer.
pub fn load_recognizer(cfg: &crate::model::SttSettings) -> Result<SharedRecognizer> {
    #[cfg(feature = "stt")]
    {
        match load_engine(cfg)? {
            Engine::Offline(r) => Ok(r),
            Engine::Streaming(_) => anyhow::bail!("model {} is a streaming model", cfg.model),
        }
    }
    #[cfg(not(feature = "stt"))]
    {
        let _ = cfg;
        anyhow::bail!("this build has no speech-to-text (built without the `stt` feature)")
    }
}

/// A simple RMS-energy voice detector. Used in tests and as a fallback when the
/// Silero model is missing.
pub struct EnergySegmenter {
    threshold: f32,
    min_silence_samples: usize,
    max_speech_samples: usize,
    pre_roll: usize,
    history: std::collections::VecDeque<f32>,
    current: Vec<f32>,
    silence_run: usize,
    in_speech: bool,
}

impl EnergySegmenter {
    pub fn new(min_silence: f32, max_speech: f32) -> Self {
        Self {
            threshold: 0.01,
            min_silence_samples: (min_silence.max(0.1) * SAMPLE_RATE as f32) as usize,
            max_speech_samples: (max_speech.max(1.0) * SAMPLE_RATE as f32) as usize,
            pre_roll: (0.2 * SAMPLE_RATE as f32) as usize,
            history: Default::default(),
            current: Vec::new(),
            silence_run: 0,
            in_speech: false,
        }
    }
}

impl Segmenter for EnergySegmenter {
    fn accept(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        let mut out = Vec::new();
        for frame in samples.chunks(320) {
            let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32).sqrt();
            let voiced = rms > self.threshold;
            if self.in_speech {
                self.current.extend_from_slice(frame);
                self.silence_run = if voiced { 0 } else { self.silence_run + frame.len() };
                if self.silence_run >= self.min_silence_samples || self.current.len() >= self.max_speech_samples {
                    let keep = self.current.len().saturating_sub(self.silence_run.saturating_sub(self.pre_roll));
                    self.current.truncate(keep);
                    out.push(std::mem::take(&mut self.current));
                    self.in_speech = false;
                    self.silence_run = 0;
                }
            } else if voiced {
                self.in_speech = true;
                self.current = self.history.drain(..).collect();
                self.current.extend_from_slice(frame);
            } else {
                self.history.extend(frame.iter().copied());
                while self.history.len() > self.pre_roll {
                    self.history.pop_front();
                }
            }
        }
        out
    }

    fn speaking(&self) -> bool {
        self.in_speech
    }

    fn flush(&mut self) -> Vec<Vec<f32>> {
        self.in_speech = false;
        if self.current.is_empty() {
            vec![]
        } else {
            vec![std::mem::take(&mut self.current)]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32) -> Vec<f32> {
        (0..(secs * SAMPLE_RATE as f32) as usize).map(|i| (i as f32 * 0.05).sin() * 0.3).collect()
    }

    #[test]
    fn energy_segments() {
        let mut s = EnergySegmenter::new(0.3, 10.0);
        let mut audio = vec![0.0; 8000];
        audio.extend(tone(1.0));
        audio.extend(vec![0.0; 8000]);
        audio.extend(tone(0.5));
        audio.extend(vec![0.0; 8000]);
        let segs = s.accept(&audio);
        assert_eq!(segs.len(), 2);
        assert!(segs[0].len() >= 16000 && segs[0].len() <= 16000 + 6400, "{}", segs[0].len());
    }
}

#[cfg(test)]
mod case_tests {
    #[test]
    fn sentence_case() {
        assert_eq!(super::sentence_case("IT IS VERY LIKE THE OLD PORTRAIT"), "It is very like the old portrait");
        assert_eq!(super::sentence_case("I DON'T KNOW I'M HERE"), "I don't know I'm here");
        assert_eq!(super::sentence_case("Already Mixed"), "Already Mixed");
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    #[test]
    fn gate_limits() {
        let g = DecodeGate::new(2);
        let a = g.try_acquire();
        let b = g.try_acquire();
        assert!(a.is_some() && b.is_some());
        assert!(g.try_acquire().is_none());
        drop(a);
        assert!(g.try_acquire().is_some());
    }
}
