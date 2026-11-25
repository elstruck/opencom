//! Per-channel pipeline: audio in, VAD, partial and final transcripts out to the hub.

use super::{DecodeGate, Recognizer, Segmenter, SharedRecognizer, SAMPLE_RATE};
use std::sync::Arc;
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
    /// The interval grows with the utterance length, because each partial decodes the whole utterance.
    pub partial_interval: f32,
    /// Emit level meters (events) this often.
    pub level_interval: Duration,
    /// Shared across channels to cap CPU use.
    pub gate: Arc<DecodeGate>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self { partial_interval: 0.8, level_interval: Duration::from_millis(100), gate: DecodeGate::new(64) }
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
        let mut stalled = false;
        loop {
            let first = match rx.recv_timeout(Duration::from_secs(3)) {
                Ok(AudioMsg::Samples(s)) => {
                    if stalled {
                        stalled = false;
                        self.hub.emit(Event::Status { component: "audio".into(), ok: true, message: format!("{}: audio is back", self.channel_name()) });
                    }
                    s
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if !stalled {
                        stalled = true;
                        self.hub.emit(Event::Level { channel_id: self.channel_id, rms: 0.0, speaking: false });
                        self.hub.emit(Event::Status {
                            component: "audio".into(),
                            ok: false,
                            message: format!("{}: no audio for 3 s (device unplugged, or in use by another app?)", self.channel_name()),
                        });
                    }
                    continue;
                }
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

    fn channel_name(&self) -> String {
        self.hub.channel(self.channel_id).map(|c| c.name).unwrap_or_else(|| self.channel_id.to_string())
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
        let base = (self.cfg.partial_interval * SAMPLE_RATE as f32) as usize;
        // Every 4 s of speech doubles the wait between partials.
        let interval = base * (1 + self.speech.len() / (4 * SAMPLE_RATE as usize));
        if base == 0 || !self.seg.speaking() || self.since_partial < interval || self.speech.len() < SAMPLE_RATE as usize / 2 {
            return;
        }
        let gate = self.cfg.gate.clone();
        let Some(_slot) = gate.try_acquire() else { return };
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
        let gate = self.cfg.gate.clone();
        let slot = gate.acquire();
        let result = self.rec.transcribe(seg);
        drop(slot);
        let text = match result {
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
            PipelineConfig { partial_interval: 0.5, level_interval: Duration::from_secs(100), ..Default::default() },
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

    /// Fake streaming session: "words" while the input is loud, endpoint after 0.5 s of quiet.
    struct FakeSession {
        text: String,
        quiet: usize,
    }
    impl crate::stt::StreamSession for FakeSession {
        fn accept(&mut self, samples: &[f32]) {
            let loud = samples.iter().any(|s| s.abs() > 0.1);
            if loud {
                self.text = "streaming words".into();
                self.quiet = 0;
            } else {
                self.quiet += samples.len();
            }
        }
        fn text(&self) -> String {
            self.text.clone()
        }
        fn is_endpoint(&self) -> bool {
            !self.text.is_empty() && self.quiet >= 8000
        }
        fn reset(&mut self) {
            self.text.clear();
            self.quiet = 0;
        }
    }

    #[test]
    fn two_pass_uses_final_model() {
        let mut cfg = ShowConfig::default();
        let ch = Channel::new("A");
        let id = ch.id;
        cfg.channels.push(ch);
        let hub = Hub::new(cfg, None);
        let mut p = StreamingPipeline::new(id, hub.clone(), Box::new(FakeSession { text: String::new(), quiet: 0 }), PipelineConfig::default())
            .with_final(Arc::new(FixedRecognizer("final".into())));
        let mut audio = vec![0.0; 8000];
        audio.extend(tone(1.0));
        audio.extend(vec![0.0; 16000]);
        for c in audio.chunks(1600) {
            p.feed(c);
        }
        let t = hub.transcript();
        assert_eq!(t.len(), 1, "{t:?}");
        assert!(!t[0].in_progress);
        // Final text comes from the offline model, and covers the speech plus pre-roll.
        assert!(t[0].text.starts_with("final"), "{}", t[0].text);
        let secs: f32 = t[0].text.split_whitespace().nth(1).unwrap().parse().unwrap();
        assert!(secs >= 1.4, "audio too short: {secs}");
    }

    #[test]
    fn clean_drops_noise() {
        assert_eq!(clean(" . "), "");
        assert_eq!(clean("[BLANK_AUDIO]"), "");
        assert_eq!(clean("Go."), "Go.");
    }
}

/// Pipeline for streaming models: text appears as it is decoded, and the model's own
/// endpoint detection ends each utterance.
pub struct StreamingPipeline {
    pub channel_id: Uuid,
    hub: SharedHub,
    session: Box<dyn super::StreamSession>,
    /// Two-pass mode: decode each finished utterance again with this model.
    final_rec: Option<SharedRecognizer>,
    audio: Vec<f32>,
    pre_roll: std::collections::VecDeque<f32>,
    cfg: PipelineConfig,
    entry: Option<Uuid>,
    last_text: String,
    last_level: Instant,
    level_acc: (f32, usize),
}

impl StreamingPipeline {
    pub fn new(channel_id: Uuid, hub: SharedHub, session: Box<dyn super::StreamSession>, cfg: PipelineConfig) -> Self {
        Self {
            channel_id,
            hub,
            session,
            final_rec: None,
            audio: Vec::new(),
            pre_roll: Default::default(),
            cfg,
            entry: None,
            last_text: String::new(),
            last_level: Instant::now(),
            level_acc: (0.0, 0),
        }
    }

    /// Use `rec` for the final text of every utterance (two-pass mode).
    pub fn with_final(mut self, rec: SharedRecognizer) -> Self {
        self.final_rec = Some(rec);
        self
    }

    pub fn run(mut self, rx: Receiver<AudioMsg>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(3)) {
                Ok(AudioMsg::Samples(s)) => self.feed(&s),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let name = self.hub.channel(self.channel_id).map(|c| c.name).unwrap_or_default();
                    self.hub.emit(Event::Status { component: "audio".into(), ok: false, message: format!("{name}: no audio for 3 s") });
                }
                Ok(AudioMsg::End) | Err(_) => break,
            }
        }
        self.finish();
    }

    pub fn feed(&mut self, samples: &[f32]) {
        self.level_acc.0 += samples.iter().map(|s| s * s).sum::<f32>();
        self.level_acc.1 += samples.len();
        self.session.accept(samples);
        let text = super::sentence_case(&self.session.text());
        if self.final_rec.is_some() {
            if text.is_empty() && self.entry.is_none() {
                // Not in an utterance yet: keep a short pre-roll.
                self.pre_roll.extend(samples.iter().copied());
                let max = (1.2 * SAMPLE_RATE as f32) as usize;
                while self.pre_roll.len() > max {
                    self.pre_roll.pop_front();
                }
            } else {
                if self.audio.is_empty() {
                    self.audio.extend(self.pre_roll.drain(..));
                }
                self.audio.extend_from_slice(samples);
            }
        }
        if self.last_level.elapsed() >= self.cfg.level_interval && self.level_acc.1 > 0 {
            let rms = (self.level_acc.0 / self.level_acc.1 as f32).sqrt();
            self.hub.emit(Event::Level { channel_id: self.channel_id, rms, speaking: !text.is_empty() });
            self.level_acc = (0.0, 0);
            self.last_level = Instant::now();
        }
        if !text.is_empty() && text != self.last_text {
            match self.entry {
                Some(id) => {
                    self.hub.update_entry(id, text.clone(), true);
                }
                None => self.entry = Some(self.hub.add_entry(self.channel_id, text.clone(), EntrySource::Audio, true).id),
            }
            self.last_text = text;
        }
        if self.session.is_endpoint() {
            self.end_utterance();
        }
    }

    fn end_utterance(&mut self) {
        let mut text = clean(&self.last_text);
        if let (Some(rec), false) = (&self.final_rec, self.audio.is_empty() || text.is_empty()) {
            let slot = self.cfg.gate.acquire();
            match rec.transcribe(&self.audio) {
                Ok(t) if !clean(&t).is_empty() => text = clean(&t),
                Ok(_) => {}
                Err(e) => tracing::warn!("final decode failed: {e:#}"),
            }
            drop(slot);
        }
        // Endpoint detection reports a little late; the next utterance may already have
        // started. Carry the tail over as the next utterance's pre-roll.
        let keep = (0.8 * SAMPLE_RATE as f32) as usize;
        let tail = self.audio.len().saturating_sub(keep);
        self.pre_roll = self.audio.drain(tail..).collect();
        self.audio.clear();
        match (self.entry.take(), text.is_empty()) {
            (Some(id), false) => {
                self.hub.update_entry(id, text, false);
            }
            (Some(id), true) => self.hub.remove_entry(id),
            _ => {}
        }
        self.last_text.clear();
        self.session.reset();
    }

    pub fn finish(mut self) {
        // Push a little silence so the last words come out, then close the utterance.
        self.session.accept(&vec![0.0; SAMPLE_RATE as usize / 2]);
        let text = super::sentence_case(&self.session.text());
        if !text.is_empty() {
            self.last_text = text.clone();
            if self.entry.is_none() {
                self.entry = Some(self.hub.add_entry(self.channel_id, text, EntrySource::Audio, true).id);
            }
        }
        self.end_utterance();
    }
}
