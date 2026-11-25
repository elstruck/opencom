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

pub use pipeline::{AudioMsg, ChannelPipeline, FixedRecognizer, PipelineConfig};

use anyhow::Result;
use std::sync::Arc;

pub const SAMPLE_RATE: u32 = 16_000;

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

/// Load a recognizer for a catalog model.
pub fn load_recognizer(cfg: &crate::model::SttSettings) -> Result<SharedRecognizer> {
    #[cfg(feature = "stt")]
    {
        let spec = catalog::find(&cfg.model).ok_or_else(|| anyhow::anyhow!("unknown model {:?} (see `opencom models`)", cfg.model))?;
        let dir = models::models_dir(cfg).join(spec.dir);
        if !dir.exists() {
            anyhow::bail!("model {} is not downloaded; run `opencom models download {}`", spec.id, spec.id);
        }
        Ok(Arc::new(sherpa::SherpaRecognizer::load(spec, &dir, cfg.threads)?))
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
