# XN Pocket TTS runtime experiment

This isolated harness evaluates `ptts` 0.2.2 with `xn` 0.1.21 before any
production provider integration. It keeps one model and one prepared voice
state warm, resets the sampling seed for every run, pipelines autoregressive
generation into a bounded two-frame decoder queue, and reports first decoded
audio, completion, RTF, deterministic PCM identity, native peak RSS where the
OS exposes it, and after-first-audio cancellation behavior.

The harness performs no downloads. Supply an already-authorized official model,
its matching tokenizer and JSON configuration, and an XN-generated voice-state
safetensors. Q8_0, Q6_K, and Q4_K weights must be XN GGUF files produced by the
pinned XN quantizer.

```console
cargo run --locked --release -- \
  --config /absolute/config.json \
  --weights /absolute/model-q8-runtime.gguf \
  --tokenizer /absolute/tokenizer.model \
  --voice-state /absolute/voice.safetensors \
  --precision q8 \
  --threads 2 \
  --warmups 2 \
  --iterations 10 \
  --cancellation-iterations 2
```

This is an engine benchmark, not an UtterPipe provider. A candidate that passes
the performance and acoustic gates must still receive a bounded protocol
adapter and pass `utterpipe-conformance` and `utterpipe-benchmark` on Windows,
macOS, and Linux.
