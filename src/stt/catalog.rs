//! Known speech models and where to download them.
//!
//! All archives come from the sherpa-onnx `asr-models` release. They unpack into a
//! folder named after the archive.

use serde::Serialize;

pub const RELEASE_BASE: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";
pub const VAD_FILE: &str = "silero_vad.onnx";

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ModelKind {
    NemoTransducer,
    Moonshine,
    Whisper,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub kind: ModelKind,
    /// Archive folder name (the archive is `{dir}.tar.bz2`).
    pub dir: &'static str,
    pub download_mb: u32,
    pub languages: &'static str,
    /// Relative file names inside `dir`: encoder/decoder/joiner/tokens or model parts.
    pub files: &'static [(&'static str, &'static str)],
}

impl ModelSpec {
    pub fn archive_url(&self) -> String {
        format!("{RELEASE_BASE}/{}.tar.bz2", self.dir)
    }

    pub fn file(&self, role: &str) -> Option<&'static str> {
        self.files.iter().find(|(r, _)| *r == role).map(|(_, f)| *f)
    }
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "parakeet-tdt-0.6b-v2-int8",
        name: "Parakeet TDT 0.6B v2 (English)",
        description: "Best English accuracy for its speed. Several channels in real time on a modern CPU.",
        kind: ModelKind::NemoTransducer,
        dir: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
        download_mb: 460,
        languages: "en",
        files: &[("encoder", "encoder.int8.onnx"), ("decoder", "decoder.int8.onnx"), ("joiner", "joiner.int8.onnx"), ("tokens", "tokens.txt")],
    },
    ModelSpec {
        id: "parakeet-tdt-0.6b-v3-int8",
        name: "Parakeet TDT 0.6B v3 (25 European languages)",
        description: "Multilingual Parakeet: English, German, French, Spanish, Italian, Dutch, Polish, and more.",
        kind: ModelKind::NemoTransducer,
        dir: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
        download_mb: 465,
        languages: "bg,cs,da,de,el,en,es,et,fi,fr,hr,hu,it,lt,lv,mt,nl,pl,pt,ro,ru,sk,sl,sv,uk",
        files: &[("encoder", "encoder.int8.onnx"), ("decoder", "decoder.int8.onnx"), ("joiner", "joiner.int8.onnx"), ("tokens", "tokens.txt")],
    },
    ModelSpec {
        id: "moonshine-tiny-en-int8",
        name: "Moonshine Tiny (English)",
        description: "Very small and fast. For laptops, Raspberry Pi class hardware, or many channels.",
        kind: ModelKind::Moonshine,
        dir: "sherpa-onnx-moonshine-tiny-en-int8",
        download_mb: 103,
        languages: "en",
        files: &[
            ("preprocessor", "preprocess.onnx"),
            ("encoder", "encode.int8.onnx"),
            ("uncached_decoder", "uncached_decode.int8.onnx"),
            ("cached_decoder", "cached_decode.int8.onnx"),
            ("tokens", "tokens.txt"),
        ],
    },
    ModelSpec {
        id: "moonshine-base-en-int8",
        name: "Moonshine Base (English)",
        description: "Small and fast, more accurate than Tiny.",
        kind: ModelKind::Moonshine,
        dir: "sherpa-onnx-moonshine-base-en-int8",
        download_mb: 240,
        languages: "en",
        files: &[
            ("preprocessor", "preprocess.onnx"),
            ("encoder", "encode.int8.onnx"),
            ("uncached_decoder", "uncached_decode.int8.onnx"),
            ("cached_decoder", "cached_decode.int8.onnx"),
            ("tokens", "tokens.txt"),
        ],
    },
    ModelSpec {
        id: "whisper-base.en",
        name: "Whisper Base (English)",
        description: "OpenAI Whisper base, English only.",
        kind: ModelKind::Whisper,
        dir: "sherpa-onnx-whisper-base.en",
        download_mb: 199,
        languages: "en",
        files: &[("encoder", "base.en-encoder.int8.onnx"), ("decoder", "base.en-decoder.int8.onnx"), ("tokens", "base.en-tokens.txt")],
    },
    ModelSpec {
        id: "whisper-small",
        name: "Whisper Small (99 languages)",
        description: "Multilingual Whisper. Slower; use when a channel needs a language Parakeet lacks.",
        kind: ModelKind::Whisper,
        dir: "sherpa-onnx-whisper-small",
        download_mb: 610,
        languages: "multilingual",
        files: &[("encoder", "small-encoder.int8.onnx"), ("decoder", "small-decoder.int8.onnx"), ("tokens", "small-tokens.txt")],
    },
];

pub fn find(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id)
}
