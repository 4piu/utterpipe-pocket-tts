UtterPipe Pocket TTS 0.2.0 replaces the v0.1 sherpa-onnx runtime and converted
January model with the native XN runtime and the verified official April 2026
English Pocket TTS model.

Highlights:

- authenticated bootstrap from Kyutai's gated Hugging Face model repository,
  exact source verification, and deterministic local Q8 conversion;
- a pinned, license-aware catalog of reference voices plus cancellable local or
  HTTP(S) WAV import;
- true incremental 24 kHz PCM16 delivery, warm-engine reuse, bounded
  backpressure, and protocol cancellation with no audio after acknowledgement;
- cross-platform validation on macOS arm64, Windows x64, and Linux x64; and
- an offline acoustic release gate calibrated against known artifact-producing
  runtimes and confirmed by retained human listening review.

The v0.1 model ID and sherpa-specific provider options are no longer accepted.
There were no external v0.1 installations requiring storage migration. Run
`models prepare`, install or import a voice, and use model
`pocket-tts-english-2026-04-q8`. Model weights and voices remain separate from
the release archives. Release binaries and tags remain unsigned, with SHA-256
files published beside every archive.
