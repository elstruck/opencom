# Changelog

## 0.1.1

- Fix: text fields and dropdowns were white with white text in the Linux desktop app (WebKitGTK drew native GTK widgets). They now follow the dark theme.
- Fix: setup pages update at once after you add, edit or delete something (no refresh needed).

## 0.1.0 (unreleased)

First version.

- Multi-channel live transcription with local sherpa-onnx models (Parakeet, Moonshine, Whisper) and Silero VAD.
- Partial (in-progress) results, a shared CPU decode budget, and an adaptive partial interval.
- Keywords with highlight, replacement and masking.
- Automations: phrase, OSC, webhook and manual triggers. OSC, MIDI, HTTP, alert, send-text, UI and delay actions.
- HTTP, WebSocket and SSE API. Embedded web UI.
- Network channels between opencom machines, with mDNS discovery.
- Tauri desktop shell with a tray menu and an always-on-top option.
- Optional ProdCom-compatible API adapter.
