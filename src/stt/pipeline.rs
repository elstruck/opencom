//! Per-channel pipeline: audio in, VAD, partial and final transcripts out to the hub.

use super::{Recognizer, Segmenter, SharedRecognizer, SAMPLE_RATE};
use crate::hub::{Event, SharedHub};
use crate::model::EntrySource;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub enum AudioMsg {
    Samples(Vec<f32>),
    End,
}

#[derive(Clone)]
pub struct PipelineConfig {
    /// Seconds of new speech between partial decodes. 0 turns partials off.
    pub partial_interval: f32,
    /// Emit level meters (events) this often.
    pub level_interval: Duration,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self { partial_interval: 0.8, level_interval: Duration::from_millis(100) }
    }
}

pub struct ChannelPipeline {
    pub channel_id: Uuid,
    hub: SharedHub,
    rec: SharedRecognizer,
    seg: Box<dyn Segmenter>,
    cfg: PipelineConfig,
    speech: Vec<f32>,
    pre_roll: std::collections::VecDeque<f32>,
    entry: Option<Uuid>,
    since_partial: usize,
    last_level: Instant,
    level_acc: (f32, usize),
}

impl ChannelPipeline {
    pub fn new(channel_id: Uuid, hub: SharedHub, rec: SharedRecognizer, seg: Box<dyn Segmenter>, cfg: PipelineConfig) -> Self {
        Self {
            channel_id,
            hub,
            rec,
            seg,
            cfg,
            speech: Vec::new(),
            pre_roll: Default::default(),
            entry: None,
            since_partial: 0,
            last_level: Instant::now(),
            level_acc: (0.0, 0),
        }
    }

    /// Run until the sender closes or sends `End`. Blocks; call from a dedicated thread.
    pub fn run(mut self, rx: Receiver<AudioMsg>) {
        loop {
            let first = match rx.recv() {
                Ok(AudioMsg::Samples(s)) => s,
                Ok(AudioMsg::End) | Err(_) => break,
            };
            self.feed(&first);
            // Catch up on anything queued before doing expensive partial decodes.
            let mut ended = false;
            loop {
                match rx.try_recv() {
                    Ok(AudioMsg::Samples(s)) => self.feed(&s),
                    Ok(AudioMsg::End) | Err(TryRecvError::Disconnected) => {
                        ended = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
            if ended {
                break;
            }
            self.maybe_partial();
        }
        for seg in self.seg.flush() {
            self.finalize(&seg);
        }
        if let Some(id) = self.entry.take() {
            self.hub.remove_entry(id);
        }
    }

    /// Feed audio synchronously (also used by tests and file transcription).
    pub fn feed(&mut self, samples: &[f32]) {
        self.levels(samples);
        let was = self.seg.speaking();
        let segs = self.seg.accept(samples);
        let now = self.seg.speaking();
        if was || now || !segs.is_empty() {
            if self.speech.is_empty() {
                self.speech.extend(self.pre_roll.iter().copied());
            }
            self.speech.extend_from_slice(samples);
            self.since_partial += samples.len();
        } else {
            self.pre_roll.extend(samples.iter().copied());
            let max = (0.3 * SAMPLE_RATE as f32) as usize;
            while self.pre_roll.len() > max {
                self.pre_roll.pop_front();
            }
        }
        for seg in segs {
            self.finalize(&seg);
        }
    }

    pub fn finish(mut self) {
        for seg in self.seg.flush() {
            self.finalize(&seg);
        }
        if let Some(id) = self.entry.take() {
            self.hub.remove_entry(id);
        }
    }

    fn levels(&mut self, samples: &[f32]) {
        self.level_acc.0 += samples.iter().map(|s| s * s).sum::<f32>();
        self.level_acc.1 += samples.len();
        if self.last_level.elapsed() >= self.cfg.level_interval && self.level_acc.1 > 0 {
            let rms = (self.level_acc.0 / self.level_acc.1 as f32).sqrt();
            self.hub.emit(Event::Level { channel_id: self.channel_id, rms, speaking: self.seg.speaking() });
            self.level_acc = (0.0, 0);
            self.last_level = Instant::now();
        }
    }

    fn maybe_partial(&mut self) {
        let interval = (self.cfg.partial_interval * SAMPLE_RATE as f32) as usize;
        if interval == 0 || !self.seg.speaking() || self.since_partial < interval || self.speech.len() < SAMPLE_RATE as usize / 2 {
            return;
        }
        self.since_partial = 0;
        let text = match self.rec.transcribe(&self.speech) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("partial decode failed: {e:#}");
                return;
            }
        };
        if text.is_empty() {
            return;
        }
        match self.entry {
            Some(id) => {
                self.hub.update_entry(id, text, true);
            }
            None => self.entry = Some(self.hub.add_entry(self.channel_id, text, EntrySource::Audio, true).id),
        }
    }

    fn finalize(&mut self, seg: &[f32]) {
        let text = match self.rec.transcribe(seg) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("decode failed: {e:#}");
                String::new()
            }
        };
        let text = clean(&text);
        match (self.entry.take(), text.is_empty()) {
            (Some(id), false) => {
                self.hub.update_entry(id, text, false);
            }
            (Some(id), true) => self.hub.remove_entry(id),
            (None, false) => {
                self.hub.add_entry(self.channel_id, text, EntrySource::Audio, false);
            }
            (None, true) => {}
        }
        self.speech.clear();
        self.since_partial = 0;
    }
}

/// Drop recognizer output that is only noise markers or punctuation.
fn clean(t: &str) -> String {
    let t = t.trim();
    let has_word = t.chars().any(|c| c.is_alphanumeric());
    if !has_word {
        return String::new();
    }
    let lower = t.to_lowercase();
    for junk in ["[blank_audio]", "(silence)", "[music]", "[noise]", "<|nospeech|>"] {
        if lower == junk {
            return String::new();
        }
    }
    t.to_string()
}

/// A recognizer for tests: returns a fixed text with the audio duration.
pub struct FixedRecognizer(pub String);

impl Recognizer for FixedRecognizer {
    fn transcribe(&self, samples: &[f32]) -> anyhow::Result<String> {
        Ok(format!("{} {:.1}", self.0, samples.len() as f32 / SAMPLE_RATE as f32))
    }
    fn name(&self) -> String {
        "fixed".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::Hub;
    use crate::model::{Channel, ShowConfig};
    use crate::stt::EnergySegmenter;
    use std::sync::Arc;

    fn tone(secs: f32) -> Vec<f32> {
        (0..(secs * SAMPLE_RATE as f32) as usize).map(|i| (i as f32 * 0.05).sin() * 0.3).collect()
    }

    #[test]
    fn partial_then_final() {
        let mut cfg = ShowConfig::default();
        let ch = Channel::new("A");
        let id = ch.id;
        cfg.channels.push(ch);
        let hub = Hub::new(cfg, None);
        let mut rx = hub.subscribe();
        let (tx, arx) = std::sync::mpsc::channel();
        let p = ChannelPipeline::new(
            id,
            hub.clone(),
            Arc::new(FixedRecognizer("hello".into())),
            Box::new(EnergySegmenter::new(0.3, 10.0)),
            PipelineConfig { partial_interval: 0.5, level_interval: Duration::from_secs(100) },
        );
        let h = std::thread::spawn(move || p.run(arx));
        // Feed in real-time-ish chunks so partials happen.
        let mut audio = vec![0.0; 4000];
        audio.extend(tone(2.0));
        audio.extend(vec![0.0; 8000]);
        for c in audio.chunks(1600) {
            tx.send(AudioMsg::Samples(c.to_vec())).unwrap();
            std::thread::sleep(Duration::from_millis(2));
        }
        tx.send(AudioMsg::End).unwrap();
        h.join().unwrap();

        let t = hub.transcript();
        assert_eq!(t.len(), 1, "{t:?}");
        assert!(!t[0].in_progress);
        assert!(t[0].text.starts_with("hello"));
        let mut saw_partial = false;
        while let Ok(e) = rx.try_recv() {
            if let Event::EntryAdded { entry } = e {
                saw_partial |= entry.in_progress;
            }
        }
        assert!(saw_partial, "expected an in-progress entry first");
    }

    #[test]
    fn clean_drops_noise() {
        assert_eq!(clean(" . "), "");
        assert_eq!(clean("[BLANK_AUDIO]"), "");
        assert_eq!(clean("Go."), "Go.");
    }
}
