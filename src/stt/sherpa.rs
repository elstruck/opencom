//! sherpa-onnx backed recognizer and Silero VAD segmenter.

use super::catalog::{ModelKind, ModelSpec};
use super::{Recognizer, Segmenter, SAMPLE_RATE};
use anyhow::{anyhow, Result};
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};
use std::path::Path;

pub struct SherpaRecognizer {
    rec: OfflineRecognizer,
    name: String,
}

impl SherpaRecognizer {
    pub fn load(spec: &ModelSpec, dir: &Path, threads: i32) -> Result<Self> {
        let f = |role: &str| -> Result<Option<String>> {
            let name = spec.file(role).ok_or_else(|| anyhow!("model {} has no {role} file", spec.id))?;
            let p = dir.join(name);
            if !p.exists() {
                return Err(anyhow!("missing model file {}", p.display()));
            }
            Ok(Some(p.to_string_lossy().to_string()))
        };
        let mut cfg = OfflineRecognizerConfig::default();
        cfg.model_config.num_threads = threads.max(1);
        cfg.model_config.tokens = f("tokens")?;
        match spec.kind {
            ModelKind::NemoTransducer => {
                cfg.model_config.transducer.encoder = f("encoder")?;
                cfg.model_config.transducer.decoder = f("decoder")?;
                cfg.model_config.transducer.joiner = f("joiner")?;
                cfg.model_config.model_type = Some("nemo_transducer".into());
            }
            ModelKind::Moonshine => {
                cfg.model_config.moonshine.preprocessor = f("preprocessor")?;
                cfg.model_config.moonshine.encoder = f("encoder")?;
                cfg.model_config.moonshine.uncached_decoder = f("uncached_decoder")?;
                cfg.model_config.moonshine.cached_decoder = f("cached_decoder")?;
            }
            ModelKind::Whisper => {
                cfg.model_config.whisper.encoder = f("encoder")?;
                cfg.model_config.whisper.decoder = f("decoder")?;
                cfg.model_config.whisper.task = Some("transcribe".into());
                cfg.model_config.whisper.tail_paddings = -1;
            }
        }
        let rec = OfflineRecognizer::create(&cfg).ok_or_else(|| anyhow!("sherpa-onnx could not load model {}", spec.id))?;
        Ok(Self { rec, name: spec.name.to_string() })
    }
}

impl Recognizer for SherpaRecognizer {
    fn transcribe(&self, samples: &[f32]) -> Result<String> {
        if samples.len() < (SAMPLE_RATE as usize / 10) {
            return Ok(String::new());
        }
        let stream = self.rec.create_stream();
        stream.accept_waveform(SAMPLE_RATE as i32, samples);
        self.rec.decode(&stream);
        let r = stream.get_result().ok_or_else(|| anyhow!("no result"))?;
        Ok(r.text.trim().to_string())
    }

    fn name(&self) -> String {
        self.name.clone()
    }
}

pub struct SileroSegmenter {
    vad: VoiceActivityDetector,
    window: usize,
    pending: Vec<f32>,
}

impl SileroSegmenter {
    pub fn new(model: &str, threshold: f32, min_silence: f32, max_speech: f32) -> Result<Self> {
        let window = 512;
        let cfg = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(model.to_string()),
                threshold,
                min_silence_duration: min_silence,
                min_speech_duration: 0.25,
                window_size: window as i32,
                max_speech_duration: max_speech,
            },
            sample_rate: SAMPLE_RATE as i32,
            num_threads: 1,
            ..Default::default()
        };
        let vad = VoiceActivityDetector::create(&cfg, (max_speech + 5.0).max(30.0)).ok_or_else(|| anyhow!("could not create Silero VAD"))?;
        Ok(Self { vad, window, pending: Vec::new() })
    }

    fn drain(&mut self) -> Vec<Vec<f32>> {
        let mut out = Vec::new();
        while !self.vad.is_empty() {
            if let Some(seg) = self.vad.front() {
                out.push(seg.samples().to_vec());
            }
            self.vad.pop();
        }
        out
    }
}

impl Segmenter for SileroSegmenter {
    fn accept(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        self.pending.extend_from_slice(samples);
        let mut off = 0;
        while self.pending.len() - off >= self.window {
            self.vad.accept_waveform(&self.pending[off..off + self.window]);
            off += self.window;
        }
        self.pending.drain(..off);
        self.drain()
    }

    fn speaking(&self) -> bool {
        self.vad.detected()
    }

    fn flush(&mut self) -> Vec<Vec<f32>> {
        self.vad.flush();
        self.drain()
    }
}
