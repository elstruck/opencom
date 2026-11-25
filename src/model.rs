//! Core data model. Everything here is plain serializable data.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Where a channel gets its audio from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[derive(Default)]
pub enum ChannelSource {
    /// No audio. The channel only carries typed and automation messages.
    #[default]
    None,
    /// One input channel of a local sound card.
    #[serde(rename_all = "camelCase")]
    Device {
        /// Device name as the OS reports it. `None` means the default input device.
        device: Option<String>,
        /// Zero-based input channel index on that device.
        channel_index: u16,
    },
    /// A WAV file, played in real time. Useful for testing and demos.
    #[serde(rename_all = "camelCase")]
    File {
        path: String,
        #[serde(default)]
        looped: bool,
    },
    /// Mirror a channel from another opencom server on the network.
    #[serde(rename_all = "camelCase")]
    Remote {
        /// Base URL, for example `http://192.168.1.20:7430`.
        url: String,
        /// Remote channel ID or name.
        channel: String,
        /// API key of the remote server, if it has one.
        #[serde(default)]
        key: Option<String>,
    },
}


#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: Uuid,
    pub name: String,
    /// Hex color, for example `#FF6B35`.
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub source: ChannelSource,
    /// BCP 47 language tag of the speech, for example `en-US`.
    #[serde(default)]
    pub language: Option<String>,
    /// Allow typed messages into this channel.
    #[serde(default = "default_true")]
    pub typing_enabled: bool,
    /// Transcription on/off without removing the source.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Keywords that apply only to this channel.
    #[serde(default)]
    pub keywords: Vec<Keyword>,
    /// Translate this channel's final lines (needs `settings.translation`).
    #[serde(default)]
    pub translate: bool,
}

fn default_true() -> bool {
    true
}

impl Channel {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            color: None,
            source: ChannelSource::None,
            language: None,
            typing_enabled: true,
            enabled: true,
            keywords: Vec::new(),
            translate: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub channel_ids: Vec<Uuid>,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Keyword {
    pub id: Uuid,
    /// Text to look for. Matching ignores case.
    pub text: String,
    /// Match whole words only. When false, any substring matches.
    #[serde(default = "default_true")]
    pub whole_word: bool,
    #[serde(default = "default_true")]
    pub highlight: bool,
    #[serde(default)]
    pub highlight_color: Option<String>,
    /// Show this text instead of the matched text.
    #[serde(default)]
    pub replacement: Option<String>,
    /// Mask the matched text with asterisks.
    #[serde(default)]
    pub sensitive: bool,
}

impl Keyword {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            text: text.into(),
            whole_word: true,
            highlight: true,
            highlight_color: None,
            replacement: None,
            sensitive: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EntrySource {
    Audio,
    Typed,
    Automation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptEntry {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub text: String,
    pub source: EntrySource,
    /// True while speech recognition still revises this entry.
    pub in_progress: bool,
    pub seen: bool,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Keywords found in the final text.
    #[serde(default)]
    pub matched_keywords: Vec<Uuid>,
    /// Translation of the final text, when the channel has translation on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation: Option<String>,
}

// ---------------------------------------------------------------------------
// Automations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionPolicy {
    /// A new trigger cancels the running action list and starts again.
    #[default]
    CancelAndRestart,
    /// A new trigger is ignored while the action list runs.
    IgnoreIfRunning,
    /// Every trigger runs its own copy of the action list.
    RunParallel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Trigger {
    /// A transcript entry contains one of the phrases.
    #[serde(rename_all = "camelCase")]
    Phrase {
        phrases: Vec<String>,
        /// Limit to these channels. Empty means all channels.
        #[serde(default)]
        channel_ids: Vec<Uuid>,
        /// Fire on partial (in-progress) text, before speech recognition finishes.
        #[serde(default)]
        early: bool,
        #[serde(default = "default_true")]
        whole_word: bool,
    },
    /// An OSC message arrives on the OSC input port.
    #[serde(rename_all = "camelCase")]
    Osc {
        /// Address pattern. `*` matches one path segment, `**` matches the rest.
        address: String,
    },
    /// A MIDI message arrives on any MIDI input (or one whose name contains `port`).
    #[serde(rename_all = "camelCase")]
    Midi {
        #[serde(default)]
        port: Option<String>,
        /// `noteOn`, `noteOff`, `controlChange` or `programChange`. `None` matches all.
        #[serde(default)]
        kind: Option<String>,
        /// MIDI channel 0..15. `None` matches all.
        #[serde(default)]
        channel: Option<u8>,
        /// Note, controller or program number. `None` matches all.
        #[serde(default)]
        number: Option<u8>,
    },
    /// An HTTP POST arrives at `/api/v1/webhooks/{path}`.
    Webhook { path: String },
    /// Only manual runs (API or UI button).
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OscArg {
    #[serde(rename = "type")]
    pub kind: OscArgKind,
    /// String form of the value. `{text}`, `{channel}` and `{phrase}` expand.
    pub value: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum OscArgKind {
    Int,
    Float,
    String,
    Bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MidiMessage {
    NoteOn { channel: u8, note: u8, velocity: u8 },
    NoteOff { channel: u8, note: u8, velocity: u8 },
    ControlChange { channel: u8, controller: u8, value: u8 },
    ProgramChange { channel: u8, program: u8 },
}

impl MidiMessage {
    pub fn to_bytes(&self) -> Vec<u8> {
        match *self {
            MidiMessage::NoteOn { channel, note, velocity } => vec![0x90 | (channel & 0x0F), note & 0x7F, velocity & 0x7F],
            MidiMessage::NoteOff { channel, note, velocity } => vec![0x80 | (channel & 0x0F), note & 0x7F, velocity & 0x7F],
            MidiMessage::ControlChange { channel, controller, value } => vec![0xB0 | (channel & 0x0F), controller & 0x7F, value & 0x7F],
            MidiMessage::ProgramChange { channel, program } => vec![0xC0 | (channel & 0x0F), program & 0x7F],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Action {
    /// Wait before the next action.
    Delay { seconds: f64 },
    /// Send one OSC message over UDP.
    #[serde(rename_all = "camelCase")]
    Osc {
        /// An OSC destination ID. When absent, `host` and `port` apply.
        #[serde(default)]
        destination_id: Option<Uuid>,
        #[serde(default)]
        host: Option<String>,
        #[serde(default)]
        port: Option<u16>,
        address: String,
        #[serde(default)]
        args: Vec<OscArg>,
    },
    /// Send one MIDI message.
    Midi {
        /// Output port name (substring match). `None` uses the first port.
        #[serde(default)]
        port: Option<String>,
        message: MidiMessage,
    },
    /// Send an HTTP request. `{text}`, `{channel}` and `{phrase}` expand in url and body.
    #[serde(rename_all = "camelCase")]
    Http {
        method: String,
        url: String,
        #[serde(default)]
        headers: Vec<(String, String)>,
        #[serde(default)]
        body: Option<String>,
    },
    /// Post a message into a channel.
    #[serde(rename_all = "camelCase")]
    SendText { channel_id: Uuid, text: String },
    /// Flash an alert banner in every connected UI.
    #[serde(rename_all = "camelCase")]
    Alert {
        text: String,
        #[serde(default = "default_alert_seconds")]
        seconds: f64,
        #[serde(default)]
        color: Option<String>,
    },
    /// Generic UI command: selectChannel, nextChannel, previousChannel, scrollToLatest,
    /// clearTranscript, markRead, fontBigger, fontSmaller, showWindow, hideWindow.
    #[serde(rename_all = "camelCase")]
    Ui {
        command: String,
        #[serde(default)]
        channel_id: Option<Uuid>,
    },
}

fn default_alert_seconds() -> f64 {
    3.0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Automation {
    pub id: Uuid,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub policy: ExecutionPolicy,
    /// Ignore new triggers for this many seconds after a run starts.
    #[serde(default)]
    pub cooldown_seconds: f64,
    pub trigger: Trigger,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OscDestination {
    pub id: Uuid,
    pub name: String,
    pub host: String,
    pub port: u16,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Speech-to-text engine settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SttSettings {
    /// Model ID from the model catalog (see `opencom models`).
    pub model: String,
    /// Folder with downloaded models. `None` uses the platform data folder.
    #[serde(default)]
    pub models_dir: Option<String>,
    #[serde(default = "default_threads")]
    pub threads: i32,
    /// Seconds between partial (in-progress) results while someone speaks. 0 turns partials off.
    #[serde(default = "default_partial")]
    pub partial_interval: f32,
    /// Optional streaming model for live (in-progress) text. When set and `model` is an
    /// offline model, opencom runs two passes: the streaming model shows words live and finds
    /// the end of each utterance; `model` then decodes the utterance once for the final text.
    #[serde(default)]
    pub partial_model: Option<String>,
    /// Silero VAD speech threshold, 0..1.
    #[serde(default = "default_vad_threshold")]
    pub vad_threshold: f32,
    /// Silence (seconds) that ends an utterance.
    #[serde(default = "default_min_silence")]
    pub min_silence: f32,
    /// Longest utterance before it is cut (seconds).
    #[serde(default = "default_max_speech")]
    pub max_speech: f32,
}

fn default_threads() -> i32 {
    2
}
fn default_partial() -> f32 {
    0.8
}
fn default_vad_threshold() -> f32 {
    0.5
}
fn default_min_silence() -> f32 {
    0.5
}
fn default_max_speech() -> f32 {
    12.0
}

impl Default for SttSettings {
    fn default() -> Self {
        Self {
            model: "parakeet-tdt-0.6b-v2-int8".into(),
            models_dir: None,
            partial_model: None,
            threads: default_threads(),
            partial_interval: default_partial(),
            vad_threshold: default_vad_threshold(),
            min_silence: default_min_silence(),
            max_speech: default_max_speech(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub stt: SttSettings,
    /// UDP port for incoming OSC. `None` turns OSC input off.
    #[serde(default)]
    pub osc_listen_port: Option<u16>,
    /// Shared secret for the HTTP API. `None` turns auth off (localhost use).
    #[serde(default)]
    pub api_key: Option<String>,
    /// Keep at most this many transcript entries in memory.
    #[serde(default = "default_history")]
    pub history_limit: usize,
    /// Serve a ProdCom-compatible API on this port (for example 24480). `None` turns it off.
    #[serde(default)]
    pub compat_prodcom_port: Option<u16>,
    #[serde(default)]
    pub translation: TranslationSettings,
}

/// Translation through any OpenAI-compatible chat API (llama.cpp, Ollama, LM Studio, vLLM, OpenAI, ...).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranslationSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Base URL that ends before `/chat/completions`, for example `http://127.0.0.1:11434/v1`.
    #[serde(default = "default_tr_endpoint")]
    pub endpoint: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_tr_model")]
    pub model: String,
    /// Language to translate into, in plain words (for example "English" or "Spanish").
    #[serde(default = "default_tr_target")]
    pub target_language: String,
}

fn default_tr_endpoint() -> String {
    "http://127.0.0.1:11434/v1".into()
}
fn default_tr_model() -> String {
    "qwen2.5:7b-instruct".into()
}
fn default_tr_target() -> String {
    "English".into()
}

impl Default for TranslationSettings {
    fn default() -> Self {
        Self { enabled: false, endpoint: default_tr_endpoint(), api_key: None, model: default_tr_model(), target_language: default_tr_target() }
    }
}

fn default_history() -> usize {
    5000
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            stt: SttSettings::default(),
            osc_listen_port: Some(9431),
            api_key: None,
            history_limit: default_history(),
            compat_prodcom_port: None,
            translation: TranslationSettings::default(),
        }
    }
}

/// A complete show configuration. This is what save, load, export and import move around.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShowConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub groups: Vec<Group>,
    /// Keywords that apply to every channel.
    #[serde(default)]
    pub keywords: Vec<Keyword>,
    #[serde(default)]
    pub automations: Vec<Automation>,
    #[serde(default)]
    pub osc_destinations: Vec<OscDestination>,
    #[serde(default)]
    pub settings: Settings,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midi_bytes() {
        assert_eq!(MidiMessage::NoteOn { channel: 1, note: 60, velocity: 100 }.to_bytes(), vec![0x91, 60, 100]);
        assert_eq!(MidiMessage::ProgramChange { channel: 15, program: 3 }.to_bytes(), vec![0xCF, 3]);
    }

    #[test]
    fn show_config_round_trip() {
        let mut cfg = ShowConfig { name: "Test".into(), ..Default::default() };
        let mut ch = Channel::new("Stage Left");
        ch.source = ChannelSource::Device { device: None, channel_index: 3 };
        cfg.channels.push(ch);
        cfg.automations.push(Automation {
            id: Uuid::new_v4(),
            name: "Cue".into(),
            enabled: true,
            policy: ExecutionPolicy::default(),
            cooldown_seconds: 0.0,
            trigger: Trigger::Phrase { phrases: vec!["go".into()], channel_ids: vec![], early: false, whole_word: true },
            actions: vec![Action::Osc {
                destination_id: None,
                host: Some("127.0.0.1".into()),
                port: Some(8000),
                address: "/cue/go".into(),
                args: vec![OscArg { kind: OscArgKind::Int, value: "1".into() }],
            }],
        });
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ShowConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn minimal_json_uses_defaults() {
        let cfg: ShowConfig = serde_json::from_str(r#"{"channels":[{"id":"6f1c6c2e-8a5e-4d0f-9b0e-0c1d2e3f4a5b","name":"A"}]}"#).unwrap();
        assert!(cfg.channels[0].typing_enabled);
        assert_eq!(cfg.channels[0].source, ChannelSource::None);
        assert_eq!(cfg.settings.osc_listen_port, Some(9431));
    }
}
