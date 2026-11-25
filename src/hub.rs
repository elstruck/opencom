//! The hub owns the live show state and the event bus.
//!
//! Every part of the app (audio, speech, automations, API, UI) talks through the hub:
//! it changes state through hub methods and listens to `Event`s.

use crate::keywords;
use crate::model::*;
use anyhow::{Context, Result};
use chrono::Utc;
use parking_lot::RwLock;
use serde::Serialize;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum Event {
    EntryAdded { entry: TranscriptEntry },
    EntryUpdated { entry: TranscriptEntry },
    TranscriptCleared { channel_id: Option<Uuid> },
    EntryRemoved { id: Uuid },
    /// Show configuration changed (channels, keywords, automations, ...).
    ConfigChanged,
    #[serde(rename_all = "camelCase")]
    Alert { text: String, seconds: f64, color: Option<String> },
    #[serde(rename_all = "camelCase")]
    Ui { command: String, channel_id: Option<Uuid> },
    #[serde(rename_all = "camelCase")]
    AutomationFired { automation_id: Uuid, name: String, cause: String },
    #[serde(rename_all = "camelCase")]
    OscReceived { address: String, args: Vec<String>, from: String },
    #[serde(rename_all = "camelCase")]
    WebhookReceived { path: String, body: String },
    #[serde(rename_all = "camelCase")]
    MidiReceived { port: String, kind: String, channel: u8, number: u8, value: u8 },
    /// Input level per channel, 0..1 RMS, about 10 times per second.
    #[serde(rename_all = "camelCase")]
    Level { channel_id: Uuid, rms: f32, speaking: bool },
    #[serde(rename_all = "camelCase")]
    Status { component: String, ok: bool, message: String },
}

pub struct Hub {
    config: RwLock<ShowConfig>,
    transcript: RwLock<VecDeque<TranscriptEntry>>,
    tx: broadcast::Sender<Event>,
    save_path: Option<PathBuf>,
}

pub type SharedHub = Arc<Hub>;

impl Hub {
    pub fn new(config: ShowConfig, save_path: Option<PathBuf>) -> SharedHub {
        let (tx, _) = broadcast::channel(4096);
        Arc::new(Self { config: RwLock::new(config), transcript: RwLock::new(VecDeque::new()), tx, save_path })
    }

    /// Load from `path` if it exists, or start empty and save there later.
    pub fn open(path: PathBuf) -> Result<SharedHub> {
        let cfg = if path.exists() {
            let raw = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?
        } else {
            ShowConfig { name: "Untitled show".into(), ..Default::default() }
        };
        Ok(Self::new(cfg, Some(path)))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub fn emit(&self, e: Event) {
        let _ = self.tx.send(e);
    }

    // ---------------- configuration ----------------

    pub fn config(&self) -> ShowConfig {
        self.config.read().clone()
    }

    pub fn read<R>(&self, f: impl FnOnce(&ShowConfig) -> R) -> R {
        f(&self.config.read())
    }

    /// Change the configuration, save it, and tell listeners.
    pub fn update<R>(&self, f: impl FnOnce(&mut ShowConfig) -> R) -> R {
        let r = {
            let mut c = self.config.write();
            f(&mut c)
        };
        if let Err(e) = self.save() {
            tracing::warn!("could not save config: {e:#}");
        }
        self.emit(Event::ConfigChanged);
        r
    }

    pub fn replace_config(&self, cfg: ShowConfig) {
        self.update(|c| *c = cfg);
    }

    pub fn save(&self) -> Result<()> {
        let Some(path) = &self.save_path else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(&*self.config.read())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn save_path(&self) -> Option<&PathBuf> {
        self.save_path.as_ref()
    }

    pub fn channel(&self, id: Uuid) -> Option<Channel> {
        self.read(|c| c.channels.iter().find(|ch| ch.id == id).cloned())
    }

    /// Find a channel by UUID or by name (ignoring case).
    pub fn resolve_channel(&self, key: &str) -> Option<Channel> {
        self.read(|c| {
            if let Ok(id) = Uuid::parse_str(key) {
                if let Some(ch) = c.channels.iter().find(|ch| ch.id == id) {
                    return Some(ch.clone());
                }
            }
            c.channels.iter().find(|ch| ch.name.eq_ignore_ascii_case(key)).cloned()
        })
    }

    /// Global keywords plus the channel's own keywords.
    pub fn keywords_for(&self, channel_id: Uuid) -> Vec<Keyword> {
        self.read(|c| {
            let mut k = c.keywords.clone();
            if let Some(ch) = c.channels.iter().find(|ch| ch.id == channel_id) {
                k.extend(ch.keywords.iter().cloned());
            }
            k
        })
    }

    // ---------------- transcript ----------------

    pub fn transcript(&self) -> Vec<TranscriptEntry> {
        self.transcript.read().iter().cloned().collect()
    }

    pub fn entry(&self, id: Uuid) -> Option<TranscriptEntry> {
        self.transcript.read().iter().rev().find(|e| e.id == id).cloned()
    }

    fn with_keywords(&self, mut e: TranscriptEntry) -> TranscriptEntry {
        let kws = self.keywords_for(e.channel_id);
        let mut ids: Vec<Uuid> = keywords::match_keywords(&e.text, kws.iter()).into_iter().map(|s| s.keyword_id).collect();
        ids.dedup();
        e.matched_keywords = ids;
        e
    }

    /// Start a new entry. Returns its ID.
    pub fn add_entry(&self, channel_id: Uuid, text: String, source: EntrySource, in_progress: bool) -> TranscriptEntry {
        let now = Utc::now();
        let e = self.with_keywords(TranscriptEntry {
            id: Uuid::new_v4(),
            channel_id,
            text,
            source,
            in_progress,
            seen: false,
            started_at: now,
            updated_at: now,
            matched_keywords: vec![],
            translation: None,
        });
        {
            let mut t = self.transcript.write();
            t.push_back(e.clone());
            let limit = self.read(|c| c.settings.history_limit.max(100));
            while t.len() > limit {
                t.pop_front();
            }
        }
        self.emit(Event::EntryAdded { entry: e.clone() });
        e
    }

    /// Change the text or progress state of an entry.
    pub fn update_entry(&self, id: Uuid, text: String, in_progress: bool) -> Option<TranscriptEntry> {
        let updated = {
            let mut t = self.transcript.write();
            let e = t.iter_mut().rev().find(|e| e.id == id)?;
            e.text = text;
            e.in_progress = in_progress;
            e.updated_at = Utc::now();
            e.clone()
        };
        let updated = self.with_keywords(updated);
        {
            let mut t = self.transcript.write();
            if let Some(e) = t.iter_mut().rev().find(|e| e.id == id) {
                e.matched_keywords = updated.matched_keywords.clone();
            }
        }
        self.emit(Event::EntryUpdated { entry: updated.clone() });
        Some(updated)
    }

    /// Attach a translation to a final entry.
    pub fn set_translation(&self, id: Uuid, text: String) -> Option<TranscriptEntry> {
        let e = {
            let mut t = self.transcript.write();
            let e = t.iter_mut().rev().find(|e| e.id == id)?;
            e.translation = Some(text);
            e.clone()
        };
        self.emit(Event::EntryUpdated { entry: e.clone() });
        Some(e)
    }

    /// Remove an entry (for example, an in-progress entry that ended up empty).
    pub fn remove_entry(&self, id: Uuid) {
        self.transcript.write().retain(|e| e.id != id);
        self.emit(Event::EntryRemoved { id });
    }

    pub fn clear_transcript(&self, channel_id: Option<Uuid>) {
        {
            let mut t = self.transcript.write();
            match channel_id {
                Some(c) => t.retain(|e| e.channel_id != c),
                None => t.clear(),
            }
        }
        self.emit(Event::TranscriptCleared { channel_id });
    }

    pub fn mark_seen(&self, channel_id: Option<Uuid>) {
        let mut t = self.transcript.write();
        for e in t.iter_mut() {
            if channel_id.map(|c| c == e.channel_id).unwrap_or(true) {
                e.seen = true;
            }
        }
    }

    pub fn unread_count(&self, channel_id: Uuid) -> usize {
        self.transcript.read().iter().filter(|e| e.channel_id == channel_id && !e.seen && !e.in_progress).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_and_keywords() {
        let mut cfg = ShowConfig::default();
        let mut ch = Channel::new("SM");
        ch.keywords.push(Keyword::new("standby"));
        let ch_id = ch.id;
        cfg.channels.push(ch);
        cfg.keywords.push(Keyword::new("go"));
        let hub = Hub::new(cfg, None);
        let mut rx = hub.subscribe();

        let e = hub.add_entry(ch_id, "Lights stand".into(), EntrySource::Audio, true);
        assert!(e.matched_keywords.is_empty());
        let e = hub.update_entry(e.id, "Lights standby, and go".into(), false).unwrap();
        assert_eq!(e.matched_keywords.len(), 2);
        assert_eq!(hub.unread_count(ch_id), 1);
        hub.mark_seen(Some(ch_id));
        assert_eq!(hub.unread_count(ch_id), 0);

        assert!(matches!(rx.try_recv().unwrap(), Event::EntryAdded { .. }));
        assert!(matches!(rx.try_recv().unwrap(), Event::EntryUpdated { .. }));
    }

    #[test]
    fn save_and_open() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("show.json");
        let hub = Hub::open(p.clone()).unwrap();
        hub.update(|c| c.channels.push(Channel::new("A")));
        let hub2 = Hub::open(p).unwrap();
        assert_eq!(hub2.config().channels.len(), 1);
        assert!(hub2.resolve_channel("a").is_some());
    }
}
