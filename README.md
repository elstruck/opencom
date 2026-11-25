# opencom

Live multi-channel transcription and voice-triggered show control for live production.
It runs on Windows, Linux and macOS, and all speech recognition runs on your own computer.

opencom listens to several audio inputs at once: intercom, talkback, PL, Dante or MADI channels,
a stage manager mic. It writes down who said what, as a chat-style feed that the whole crew can read.
When someone says a chosen phrase, opencom can fire an OSC cue, send MIDI, call a webhook,
or flash an alert on every screen.

> **Status:** early (v0.1). The core works and has tests. Expect rough edges.

## Features

- **Multi-channel transcription.** Each channel listens to one input of a sound card.
  Every channel has its own voice detection, and the feed shows text in progress while people talk.
- **Local, fast speech recognition** through [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx):
  - NVIDIA **Parakeet TDT 0.6B**: English (v2) or 25 European languages (v3).
    About 20× faster than real time on 2 CPU threads.
  - **Moonshine** tiny and base: very small and fast, for laptops or many channels.
  - **Whisper** base and small, for other languages.
- **Keywords.** Highlight words or phrases, show a replacement text, or mask words.
- **Automations.** A trigger runs a list of actions:
  - Triggers: a spoken phrase (optionally before the sentence ends), incoming OSC (with wildcards), an HTTP webhook, or a manual run.
  - Actions: OSC, MIDI, HTTP request, post text into a channel, full-screen alert, UI commands, delay.
  - Policies: restart if running, ignore if running, run in parallel. An optional cooldown limits repeat firing.
- **Typed messages.** Anyone can type into a channel. This lets FOH reply without talking.
- **Web UI.** Any browser on the network can follow the show. The `serve` command starts it.
- **HTTP + WebSocket + SSE API** for Bitfocus Companion, Stream Deck, scripts and custom dashboards.
- **OSC input.** Use `/opencom/send <channel> <text>` and `/opencom/alert <text>`. Any address can also trigger an automation.
- **Show files.** The whole setup is one JSON file that you can save, load and share between venues.

## Quick start

```sh
# Build (needs Rust: https://rustup.rs)
cargo build --release

# Download a speech model (Parakeet v2, about 460 MB) and the voice detector
./target/release/opencom models download parakeet-tdt-0.6b-v2-int8

# List your audio inputs
./target/release/opencom devices

# Start the server and open the UI
./target/release/opencom serve --open
```

Then open **Channels**, add one channel per input, and pick the sound card and input number.

To let other computers on the network follow the show:

```sh
opencom serve --host 0.0.0.0 --port 7420
```

Set an API key in **Settings** first. Clients then pass it as `Authorization: Bearer <key>`
or as `?key=<key>` in the URL.

### Linux packages

```sh
sudo apt install build-essential pkg-config libasound2-dev libssl-dev
```

### Try it without a sound card

Set a channel's source to **WAV file**, or load `examples/theatre-show.json` and change
the sources to files. The `transcribe` command tests a model on one file:

```sh
opencom transcribe some-speech.wav --model moonshine-tiny-en-int8
```

## Choosing a model

| Model | Download | Speed | Use it for |
|---|---|---|---|
| `parakeet-tdt-0.6b-v2-int8` (default) | 460 MB | very fast | English, best accuracy |
| `parakeet-tdt-0.6b-v3-int8` | 465 MB | very fast | 25 European languages |
| `moonshine-tiny-en-int8` | 103 MB | fastest | many channels, small machines |
| `moonshine-base-en-int8` | 240 MB | fastest | English on small machines |
| `whisper-base.en` | 199 MB | fast | English, Whisper behavior |
| `whisper-small` | 610 MB | slower | other languages |

Every channel shares the one loaded model. Silent channels cost almost nothing,
because opencom only decodes audio that its voice detector (Silero VAD) marks as speech.

## API

The server listens on `http://127.0.0.1:7420` by default. All endpoints are under `/api/v1`.

| Method | Path | Purpose |
|---|---|---|
| GET | `/status` | Version, model, errors, unread counts |
| GET / PUT | `/config` | The whole show file |
| GET / PUT | `/settings` | Settings (partial updates allowed) |
| GET / POST | `/channels` | List or create channels |
| GET / PUT / DELETE | `/channels/{id-or-name}` | Read, update (partial), or delete one channel |
| POST | `/channels/{id-or-name}/send` | `{"text": "..."}` posts a typed message |
| POST | `/channels/{id-or-name}/clear`, `/read` | Clear the channel, or mark it read |
| CRUD | `/groups`, `/keywords`, `/automations`, `/osc/destinations` | Same pattern as channels |
| POST | `/automations/{id-or-name}/run` | Run an automation now |
| POST | `/osc/send` | `{"host","port","address","args":[{"type":"int","value":"1"}]}` |
| GET / POST | `/midi/ports`, `/midi/send` | List MIDI ports, or send a message |
| GET | `/audio/devices` | List sound card inputs |
| GET / POST | `/models`, `/models/{id}/download` | List models, or download one |
| GET | `/transcript?channel=&since=&limit=` | Transcript entries |
| GET | `/transcript/export?format=txt\|json` | Export the transcript |
| POST | `/transcript/clear`, `/transcript/read` | Clear, or mark read (optional `{"channelId"}`) |
| POST | `/alert` | `{"text","seconds","color"}` flashes every UI |
| POST | `/ui/{command}` | `scrollToLatest`, `nextChannel`, `fontBigger`, and others |
| POST | `/webhooks/{path}` | Fires automations that use a matching webhook trigger |
| GET | `/events` | WebSocket stream of every event, as JSON |
| GET | `/events/sse?channel=` | Server-Sent Events stream of transcript updates |

Every `{id}` accepts a UUID or a name (case does not matter).

## How it works

```
sound card ──► capture thread ──► per-channel resample to 16 kHz
                                     │
                                     ▼
                          Silero VAD (per channel) ──► partial decode every 0.8 s
                                     │                      │
                                     ▼                      ▼
                          end of utterance ──► final decode ──► hub (state + event bus)
                                                                  │
                     automation engine ◄──────────────────────────┤
                     (OSC / MIDI / HTTP / alerts)                 ├──► WebSocket / SSE / REST
                                                                  └──► show file (JSON)
```

## Roadmap

- Desktop app shell (Tauri) with a tray icon and an always-on-top window
- Sharing channels between opencom machines on the LAN (mDNS)
- Translation of channels into the reader's language
- Streaming (word-by-word) models such as Zipformer
- MIDI input triggers
- Signed Windows and macOS installers

## About this project

opencom is an independent open-source project. It is not affiliated with, or endorsed by,
any commercial product. Its code was written from scratch from public descriptions of how
live-production transcription tools behave.

## License

MIT. See [LICENSE](LICENSE). Speech models have their own licenses (NVIDIA Parakeet: CC-BY-4.0;
Moonshine: MIT; Whisper: MIT; Silero VAD: MIT).
