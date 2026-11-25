# Architecture

opencom is one Rust crate with a library (`src/lib.rs`) and a CLI (`src/main.rs`).
The desktop app (`desktop/`) is a Tauri shell that starts the same server inside the app.

## Modules

| Module | Job |
|---|---|
| `model` | Plain data: channels, groups, keywords, automations, settings, the show file. |
| `hub` | Owns live state (config and transcript) and the event bus (`tokio::broadcast`). Saves the show file on every change. |
| `keywords` | Case-insensitive phrase matching, whole-word rules, display masking. |
| `automation` | Watches hub events, matches triggers, runs action lists with execution policies. |
| `osc`, `midi` | OSC over UDP (send, listen, address patterns) and MIDI output. |
| `audio` | Sound card capture (cpal), WAV file sources, resampling to 16 kHz mono. |
| `stt` | Model catalog and downloads, sherpa-onnx recognizer, Silero VAD, per-channel pipeline, decode gate. |
| `runtime` | Turns the config into running captures and pipelines. Rebuilds on change. Retries failures every 10 s. |
| `network` | mDNS discovery, and remote channels that mirror another opencom over WebSocket. |
| `api` | HTTP, WebSocket and SSE API, and the embedded web UI (`web/`). |
| `compat` | Optional ProdCom-compatible API adapter. Off by default. |
| `server` | Starts all of the above. The CLI and the desktop app both use it. |

## Data flow

1. A capture thread reads interleaved frames from one sound card. It splits the frames by
   channel index, resamples each one to 16 kHz, and sends 100 ms chunks to the channel's pipeline.
2. Each pipeline thread runs Silero VAD. While someone speaks, it decodes the utterance so far
   every `partialInterval` seconds and posts an in-progress entry. When the utterance ends,
   it decodes the full segment and finalizes the entry.
3. The hub stores entries, matches keywords, and broadcasts `entryAdded` and `entryUpdated` events.
4. The automation engine and every WebSocket client receive those events.

## CPU budget

All channels share one loaded model. `DecodeGate` limits how many decodes run at once
(about half the CPU cores). Final decodes wait for a slot. Partial decodes are skipped
when no slot is free. A busy machine therefore loses in-progress text, never final text.

## Threads

- One capture thread per sound card or WAV file.
- One pipeline thread per channel. Decoding blocks, so it stays off the async runtime.
- Tokio runs the HTTP server, the OSC listener, the automation engine and the remote mirrors.
