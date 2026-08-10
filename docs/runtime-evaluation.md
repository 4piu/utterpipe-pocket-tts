# Pocket runtime evaluation

This experiment compares runtimes before replacing the released sherpa-onnx
backend. A candidate is not selected only because it can load a newer model: it
must preserve the provider's cross-platform, offline, incremental, cancellable,
and single-executable behavior.

## Current benchmark support

The generic UtterPipe conformance runner verifies framing and synthesis but is
not a performance benchmark. The ignored real-model test verifies genuine
incremental output, cancellation, warm reuse, and asset leases; it does not
produce reusable measurements.

This branch adds `examples/runtime_benchmark.rs` for the current backend. It
measures engine load, time to first callback audio, completion latency, output
duration, real-time factor, callback count, and deterministic PCM identity. It
uses already-prepared provider storage so downloads and archive extraction are
never mixed into synthesis measurements:

```text
SHERPA_ONNX_ARCHIVE_DIR=/absolute/sherpa-onnx-prebuilt \
  cargo run --release --example runtime_benchmark -- \
  --voice voice-zero-caro-davy --warmup 2 --iterations 10 --threads 2
```

The result is versioned JSON. It intentionally does not claim to measure whole
process startup, peak memory, GPU memory, or energy yet. Those measurements need
an outer platform-specific sampler.

A 2026-08-10 harness smoke on the development Apple-silicon Mac used the pinned
INT8 model, two threads, one warmup, and five measured runs. Engine creation was
274 ms; warm p50 first audio was 498 ms, completion was 702 ms, and real-time
factor was 0.135 for 5.2 seconds of output. The input was a generated synthetic
reference suitable for validating timing and determinism, not voice quality.

## Candidate set

| Runtime | Role in evaluation | Multilingual / 24-layer | Deployment concern |
| --- | --- | --- | --- |
| Current sherpa-onnx 1.13.4 | Released CPU baseline | No; January English only | Proven static cross-platform integration |
| Official `pocket-tts` Python package | Quality and behavior reference | Yes / yes | Python and PyTorch prevent the current single-executable distribution |
| `KevinAHM/pocket-tts-onnx` | ONNX multilingual functional oracle | Yes / yes | Python wrapper; CPU and CUDA only; different layout from sherpa |
| `audio.cpp` Pocket implementation | Native cross-platform candidate | Current packages cover 6-layer en/de/it/pt/es; 24-layer coverage must be proven | Large fast-moving multi-model GGML runtime |
| `PocketTTS.cpp` | Small native implementation reference | Predates upstream v2; no demonstrated 24-layer support | Attractive C FFI and streaming surface, but needs v2 work |
| `pocket-tts-mlx` | Apple Silicon comparison | Current support must be checked per model | macOS-only and Python-facing, so not the primary backend |

The first comparison should retain the official Python implementation as the
quality oracle, use KevinAHM's bundles to prove multilingual/24-layer behavior,
and benchmark native `audio.cpp` plus a narrow v2-capable ONNX runtime. Do not
make MLX or a Python subprocess the provider's only runtime.

## Artifact observations

Public Hugging Face metadata at revision
`4c8ad48f8a003909bc4f1122cbe88a4252124621` reports approximately 219 MB for an
official 6-layer `model.safetensors` and 672 MB for a 24-layer model. The
minimal INT8 file set in KevinAHM's ONNX bundles is approximately 165 MB for a
6-layer model and 394 MB for a 24-layer model. These are model payloads, not
complete installed-runtime sizes. Shared files and content-addressed storage
can avoid duplication when several languages are installed.

The official repository contains two different kinds of safetensors:

- `languages/<language>/model.safetensors` contains model weights;
- `languages/<language>/embeddings/*.safetensors` contains a precomputed voice
  state/KV cache tied to that model generation and language.

They are inputs for the official implementation and compatible loaders such as
audio.cpp. They are not directly compatible with the current sherpa backend.
The official gated model page should be cited as model provenance and terms,
but the quick start must not imply that manually downloading one file installs
a usable model. If the future provider consumes these weights directly, its
catalog/download flow must support explicit Hugging Face authentication and
license acceptance.

## Measurement matrix

Use the same consented reference recordings, language-matched texts, seeds,
warmup count, and measured iterations for every runtime. Evaluate at least:

- English 6-layer and one non-English 6-layer model;
- one non-English 24-layer model;
- CPU INT8/Q8 and native precision;
- CUDA native precision and quantized execution where NVIDIA hardware exists;
- Metal/MLX and native CPU on Apple Silicon where supported.

Record cold process and model load separately from first uncached voice
conditioning and warm synthesis. For every case collect time to first playable
audio, completion latency, generated duration, real-time factor, peak resident
memory, model/runtime disk bytes, and cancellation latency. Sample CPU package
energy, GPU energy, and wall energy when the platform exposes trustworthy
counters. Report medians and p95 values rather than a single best run.

GPU execution is not presumed faster or more efficient. Pocket TTS is a small,
batch-one autoregressive workload; accelerator launch, synchronization, and
host/device transfer overhead can dominate. The larger 24-layer models may
change that balance, so CPU/GPU and INT8/native-precision combinations must be
measured rather than inferred.

## Selection gates

A replacement backend must pass waveform validity and subjective language
review before performance ranking. It must then demonstrate genuine streaming,
bounded backpressure, prompt cancellation, deterministic seeded operation where
claimed, repeatable model/voice loading, and clean process shutdown. Release
selection also considers runtime binary size, native dependency provenance,
supported targets, maintenance activity, and the cost of tracking upstream
model-format changes.
