# Pocket runtime evaluation

This experiment compares runtimes before replacing the released sherpa-onnx
backend. A candidate is not selected only because it can load a newer model: it
must preserve the provider's cross-platform, offline, incremental, cancellable,
and single-executable behavior. A release candidate must support Windows,
macOS, and Linux; a platform-specific port may be useful for investigation but
does not enter the shortlist.

## Current benchmark support

The generic UtterPipe conformance runner verifies framing and synthesis but is
not a performance benchmark. The ignored real-model test verifies genuine
incremental output, cancellation, warm reuse, and asset leases; it does not
produce reusable measurements.

This branch adds `examples/runtime_benchmark.rs` for current-backend diagnostics.
It measures engine load, time to first callback audio, completion latency,
output duration, real-time factor, callback count, and deterministic PCM
identity. It uses already-prepared provider storage so downloads and archive
extraction are never mixed into synthesis measurements:

```text
SHERPA_ONNX_ARCHIVE_DIR=/absolute/sherpa-onnx-prebuilt \
  cargo run --release --example runtime_benchmark -- \
  --voice voice-zero-caro-davy --warmup 2 --iterations 10 --threads 2
```

The result is versioned JSON. It intentionally does not claim to measure whole
process startup, peak memory, GPU memory, or energy yet. Those measurements need
an outer platform-specific sampler.

Most synthesis measurements are provider-neutral and should become an
`utterpipe-benchmark` developer command: process startup, initialization, first
incremental audio, completion, output duration, real-time factor, byte/frame
counts, cancellation, and shutdown. That runner can compare Pocket TTS, eSpeak
NG, and future local providers through the same wire contract. Keep this small
provider-internal harness for measurements the protocol cannot separate, such
as native engine construction and model loading. The two layers must share a
versioned result schema where their measurements overlap.

A 2026-08-10 harness smoke on the development Apple-silicon Mac used the pinned
INT8 model, two threads, one warmup, and five measured runs. Engine creation was
274 ms; warm p50 first audio was 498 ms, completion was 702 ms, and real-time
factor was 0.135 for 5.2 seconds of output. The input was a generated synthetic
reference suitable for validating timing and determinism, not voice quality.

The provider-neutral `utterpipe-benchmark` prototype was then run on the same
Apple M4 host with two warmups, ten measured runs, and the same 60-code-point
text. RSS is the direct provider process sampled every 20 ms; its peak is a
lower bound. Byte sizes are logical file sizes.

| Provider / delivery | Initialize | First audio p50 / p95 | Completion p50 / p95 | RTF p50 / p95 | Steady / sampled-peak RSS | Executable | Provider data | Cache |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| eSpeak NG complete WAV | 22 ms | Not available | 15.9 / 17.0 ms | 0.00505 / 0.00540 | 4.39 / 4.77 MB | 20.28 MB | 0 | 18.46 MB |
| Pocket complete WAV | 605 ms | Not available | 690 / 709 ms | 0.1328 / 0.1363 | 670.61 / 671.61 MB | 22.49 MB | 198.55 MB | 98.34 MB |
| Pocket incremental PCM | 590 ms | 491 / 518 ms | 694 / 727 ms | 0.1334 / 0.1397 | 669.04 / 670.48 MB | 22.49 MB | 198.55 MB | 98.34 MB |

The generic Pocket incremental result closely matches the internal harness:
491 versus 498 ms first audio, 694 versus 702 ms completion, and 0.133 versus
0.135 RTF. This is evidence that protocol framing and the sampler do not
materially distort this workload. The large approximately 670 MB resident
footprint is a more important engine-selection result than the 22.5 MB provider
executable. Provider data contains the installed model and normalized synthetic
voice; the separately removable cache contains the downloaded model archive.

Two after-first-audio Pocket cancellation attempts were both accepted. Median
acknowledgement was 0.056 ms and terminal cleanup was 62.1 ms after the request,
with zero audio frames observed after acknowledgement. Two zero-delay eSpeak
cancellations were also accepted; median acknowledgement was 0.089 ms and
terminal cleanup was 0.308 ms. These cancellation trials measure control and
cleanup behavior, not voice quality.

## Candidate set

| Runtime | Role in evaluation | Three-platform evidence | Deployment concern |
| --- | --- | --- | --- |
| Current sherpa-onnx 1.13.4 | Released CPU baseline | Upstream explicitly supports Windows, macOS, and Linux; current provider artifacts still require real synthesis verification on each target | Proven static cross-platform integration |
| `audio.cpp` Pocket implementation | Native candidate | Documents native Windows, macOS, and Linux builds | Large fast-moving multi-model GGML runtime; evaluate a Pocket-only composite build |
| `PocketTTS.cpp` | Compact native candidate | Documents Windows, macOS, and Linux CMake builds | Attractive C FFI and streaming surface, but model-generation coverage must be proven after engine selection |

Documentation is only admission evidence. Before selection, a candidate must
build and complete real synthesis on the development Mac, Windows 11 machine,
and Manjaro Linux machine. Platform-specific runtimes such as MLX are excluded.
The official Python implementation may be used as an acoustic behavior oracle,
but it is not a provider-engine candidate unless it independently meets the
same distribution and platform gates.

Do not add a community model repository or promise a model catalog for a
candidate engine before the engine decision. Once the runtime is selected,
choose only model artifacts whose architecture, tokenizer, voice state, license,
integrity, and output have been verified with that exact runtime.

## Artifact observations

Public Hugging Face metadata at revision
`4c8ad48f8a003909bc4f1122cbe88a4252124621` reports approximately 219 MB for an
official 6-layer `model.safetensors` and 672 MB for a 24-layer model. These are
illustrative upstream model payloads, not selected provider artifacts or
complete installed-runtime sizes. Selecting and integrating converted model
repositories is deferred until after the engine decision.

The official repository contains two different kinds of safetensors:

- `languages/<language>/model.safetensors` contains model weights;
- `languages/<language>/embeddings/*.safetensors` contains a precomputed voice
  state/KV cache tied to that model generation and language.

They are inputs for the official implementation, not directly compatible with
the current sherpa backend. The official gated model page should be cited as
upstream provenance and terms, but the quick start must not imply that manually
downloading one file installs a usable model. If the selected future engine
consumes these weights directly, its catalog/download flow must support explicit
Hugging Face authentication and license acceptance.

## Measurement matrix

Use the same consented reference recordings, language-matched texts, seeds,
warmup count, and measured iterations for every runtime. Evaluate at least:

- real synthesis on Windows, macOS, and Linux for every candidate;
- one lightweight model/precision profile supported by every candidate;
- additional language, quality, or model-depth profiles only after compatible
  artifacts have been selected for the winning engine;
- CPU INT8/Q8 and native precision;
- CUDA native precision and quantized execution where NVIDIA hardware exists;
- Metal and native CPU on Apple Silicon where the candidate supports them.

Record cold process and model load separately from first uncached voice
conditioning and warm synthesis. For every release build record the provider
executable size, every shipped DLL/dylib/shared object, compressed package size,
and total installed runtime bytes without model or voice assets. For every run
collect time to first playable audio, completion latency, generated duration,
real-time factor, steady and peak resident memory, model-resident memory, GPU
memory where applicable, persistent cache growth, and cancellation latency.
Sample CPU package energy, GPU energy, and wall energy when the platform exposes
trustworthy counters. Report medians and p95 values rather than a single best
run.

GPU execution is not presumed faster or more efficient. Pocket TTS is a small,
batch-one autoregressive workload; accelerator launch, synchronization, and
host/device transfer overhead can dominate. The larger 24-layer models may
change that balance, so CPU/GPU and INT8/native-precision combinations must be
measured rather than inferred.

## Selection gates

A replacement backend must produce valid, intelligible audio without severe
artifacts before performance ranking; maximum subjective fidelity is not the
primary objective. Where the selected engine supports meaningful speed, memory,
and quality profiles, preserve that choice for users instead of imposing one
quality level. The backend must also demonstrate genuine streaming, bounded
backpressure, prompt cancellation, deterministic seeded operation where
claimed, repeatable model/voice loading, and clean process shutdown.

Selection requires verified real synthesis on Windows, macOS, and Linux. The
scorecard includes final bundled executable and companion-library bytes,
installed runtime bytes, cold and warm resident memory, accelerator memory,
cache growth, native dependency provenance, maintenance activity, and the cost
of tracking upstream model-format changes. A smaller or faster engine can beat a
higher-fidelity engine when both clear the intelligibility floor; users should
retain model/precision quality choices where the chosen runtime can offer them
reliably.
