# Provenance

opencom is an independent, clean-room project.

- The code was written from scratch. No code, assets or binaries from any commercial
  product were copied into this repository.
- The feature set comes from public descriptions of what live-production transcription
  tools do: multi-channel transcription, keyword automation, OSC and MIDI, and network sharing.
- The optional `compat` module (`src/compat.rs`) accepts the request and response *shapes*
  (paths and JSON field names) of the ProdCom Application API, so that existing integrations
  keep working. Those shapes come from that API's published OpenAPI description. The
  description is labeled MIT, and no text from it was copied.
- OSC addresses in `examples/` come from each vendor's public documentation (QLab, ETC Eos).

"ProdCom" is a trademark of its owner. opencom is not affiliated with it or endorsed by it.
