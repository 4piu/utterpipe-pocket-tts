# XN Pocket TTS runtime experiment

This isolated harness evaluates `ptts` 0.2.2 with `xn` 0.1.21 before any
production provider integration. It keeps one model and one prepared voice
state warm, resets the sampling seed for every run, pipelines autoregressive
generation into a bounded two-frame decoder queue, and reports first decoded
audio, completion, RTF, deterministic PCM identity, native peak RSS where the
OS exposes it, and after-first-audio cancellation behavior.

The dependency is pinned to project fork commit
`4dbd8d6832cf4e093d08a1bd4666a08783345e7b`, stacked on fork commit
`52493c0fbc9f52916d93df2456fe75ea71da9a58` and based on upstream commit
`4398678425e1b3d48d525024257830aec989bc58`. The fork adds backward-compatible
support for checkpoints that insert a learned BOS marker before voice
conditioning and for Mimi checkpoints whose downsample/upsample bottlenecks do
not match the outer model dimension. Configurations that omit these options
retain the upstream January 2026 behavior.

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
  --temperature 0.3 \
  --pad-with-spaces-for-short-inputs false \
  --frames-after-eos-offset 2 \
  --threads 2 \
  --warmups 2 \
  --iterations 10 \
  --cancellation-iterations 2
```

This is an engine benchmark, not an UtterPipe provider. A candidate that passes
the performance and acoustic gates must still receive a bounded protocol
adapter and pass `utterpipe-conformance` and `utterpipe-benchmark` on Windows,
macOS, and Linux.
