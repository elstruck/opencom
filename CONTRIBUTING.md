# Contributing

Thank you for helping. Bug reports, show files that break things, and pull requests are welcome.

## Build and test

```sh
cargo test                           # unit and API tests (no audio hardware needed)
cargo build --release                # CLI and server
cargo build --release -p opencom-desktop   # desktop app
```

To test speech recognition, download a small model and run it on a WAV file:

```sh
cargo run --release -- models download moonshine-tiny-en-int8
cargo run --release -- transcribe speech.wav --model moonshine-tiny-en-int8
```

## Ground rules

- Clean room: do not paste code, assets or text from closed-source products. See `docs/PROVENANCE.md`.
- Keep the web UI free of build steps: plain HTML, CSS and JS in `web/`.
- New behavior needs a test. The `api`, `automation` and `stt::pipeline` tests show the patterns.
- Run `cargo test` and `cargo build` with no warnings before you open a pull request.
