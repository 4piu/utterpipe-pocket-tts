# UtterPipe Pocket TTS provider specification

Status: implementation specification for the active XN backend.

## 1. Scope

`utterpipe-pocket-tts` is an offline neural TTS provider for UtterPipe v1. It
ships as one provider executable and offers:

- one curated, reproducible English Pocket TTS model profile;
- authenticated download of pinned official source weights;
- deterministic local Q8 conversion for the pinned XN runtime;
- local or curated reference-voice import;
- complete PCM16 WAV and incremental raw PCM16 delivery;
- bounded warm inference, cancellation, atomic storage, and management
  operations on macOS, Linux, and Windows.

The executable contains no model weights or reference voice recordings.

The first release profile deliberately does not promise arbitrary Pocket TTS
checkpoints, arbitrary GGUF files, a Python environment, GPU execution,
multilingual synthesis, model training, voice redistribution, or an in-provider
audio device. Additional profiles require their own compatibility and acoustic
evidence before catalog inclusion.

## 2. Pinned runtime

The active engine is the native Rust XN Pocket TTS implementation:

| Component | Pin |
| --- | --- |
| `ptts` fork | `4dbd8d6832cf4e093d08a1bd4666a08783345e7b` |
| `ptts` package version | `0.2.2` |
| `xn` | `0.1.21` |
| SentencePiece binding | `0.13.2` |
| precision | GGUF `q8_0` for selected weight tensors |
| execution | CPU |

The fork adds learned voice-BOS handling and asymmetric Mimi bottleneck support
needed by the April 2026 checkpoint. The former sherpa-onnx adapter and its ONNX
catalog are intentionally removed: linking both runtimes also introduced
duplicate native Protobuf implementations, while the old catalog is not
compatible with XN.

## 3. Curated model profile

The only quick-start model ID is:

```text
pocket-tts-english-2026-04-q8
```

Its source is `kyutai/pocket-tts` at revision
`19f95fe2df36e79fbd9f10008595cc4c977a0fcc`.

### 3.1 Source inputs

| Repository path | Bytes | SHA-256 |
| --- | ---: | --- |
| `languages/english_2026-04/model.safetensors` | 219,029,196 | `473f47d99560bd50eb8b4509d3cacfe7f316ab20bdca86505403a2e6a936a6e9` |
| `languages/english_2026-04/tokenizer.model` | 59,339 | `d461765ae179566678c93091c5fa6f2984c31bbe990bf1aa62d92c64d91bc3f6` |

The source is gated. Network preparation requires explicit acceptance of the
model page's access agreement outside this program and a Hugging Face token.
The provider resolves `HF_TOKEN`, `HF_TOKEN_PATH`, `HF_HOME`,
`XDG_CACHE_HOME`, then the ordinary home cache. Credentials are bounded, never
logged, and sent only to pinned Hugging Face HTTPS download URLs.

### 3.2 Conversion output

Conversion sorts source tensor names, excludes only the provider-pinned
non-runtime tensors, quantizes the selected matrix weights to GGUF Q8_0, keeps
the required remaining tensors in their defined precision, and rejects any
result not matching the expected artifact:

| Bundle file | Bytes | SHA-256 |
| --- | ---: | --- |
| `model.gguf` | 148,242,752 | `a9548b363f990faca0614dc0533d80b11be80ad0b6ac781b6f42a58dd1659ece` |
| `tokenizer.model` | 59,339 | `d461765ae179566678c93091c5fa6f2984c31bbe990bf1aa62d92c64d91bc3f6` |
| `config.json` | 1,279 | `10cf232cb3bbefa3862da21fb5d051f8c76fb9abbcfa7f2357f5a19c917ee535` |

The conversion is CPU-only, writes private staging files, checks cancellation
between tensors, verifies the exact output, and publishes nothing until the
complete bundle passes validation and a native model load.

The pinned behavior profile is:

| Field | Value |
| --- | ---: |
| sampling temperature | `0.3` |
| output gain | `0.65` |
| short-input space padding | `false` |
| semicolon removal | `false` |
| frames after EOS offset | `2` |

These values are compatibility data, not user-facing tuning controls. They are
bound into the self-describing manifest and retained acoustic evidence.

### 3.3 Disclosures

Preparation requires both manifest disclosure IDs:

1. `cc-by-4.0` — Creative Commons Attribution 4.0;
2. `pocket-tts-acceptable-use` — the upstream prohibited-use conditions linked
   from the pinned model page.

Interactive prompts default to no. Non-interactive callers supply every ID and
an explicit confirmation. Acceptance is recorded in installed metadata but
does not replace the user's compliance obligations.

## 4. Bundle contract

An XN bundle is an absolute directory containing:

```text
manifest.json
config.json
model.gguf
tokenizer.model | tokenizer.json
```

Exactly one tokenizer representation is allowed. `manifest.json` uses schema
`utterpipe.pocket-tts.xn-bundle/1` and includes model identity, source revision,
runtime identity, precision/compatibility, languages, behavior, disclosures,
and exact size/SHA-256 metadata for every payload file. Unknown manifest fields,
unexpected files declared as runtime inputs, relative roots, links, malformed
tokenizers/configs, incompatible runtime pins, excessive sizes, and mismatched
hashes fail closed.

`models prepare` constructs the one catalog bundle. `models import` accepts an
already constructed bundle for expert/offline use, but imported bundles do not
become advertised catalog compatibility.

## 5. Provider and utterance options

Runtime provider options are a closed JSON object:

| Field | Type | Required | Rules |
| --- | --- | --- | --- |
| `model` | string | yes | exactly `pocket-tts-english-2026-04-q8` |
| `voice` | string | yes | imported voice ID, 1–64 ASCII characters matching the published pattern |
| `num_threads` | integer | no | `1..64`; default 4 on arm64, 8 elsewhere |

Management sessions accept the same partial object so they can report and
prepare missing selections.

The closed per-utterance options object exposes only:

| Field | Type | Default | Rules |
| --- | --- | ---: | --- |
| `seed` | unsigned integer | `42` | `0..4294967295` |

Speed is not supported by the XN engine and is rejected as an unknown field.
The schema is returned during runtime initialization.

## 6. Reference voices

The provider accepts an absolute/relative local path in the direct CLI, an
explicit HTTP(S) URL in the direct CLI, or an absolute host-supplied path in the
framed management protocol. Imported content must be a regular classic
RIFF/WAVE file with:

- mono PCM16 samples;
- sample rate 16,000–48,000 Hz;
- duration 1–30 seconds;
- valid, bounded chunk structure.

Large containers are streamed rather than loaded whole. A 5 MiB threshold is a
warning only, not a model limit. Downloads use the system proxy and remain
cancellable. The provider normalizes the accepted reference, stores hashes and
consent metadata, and prepares a model-specific XN voice state. It never stores
arbitrary URL credentials or the original local path.

The embedded curated catalog pins each upstream repository revision, path,
byte count, SHA-256, license, and attribution. Listing is offline. Multi-select
download validates all selected files before importing any, while each final
voice publication remains atomic.

Language comes from the model. A reference voice influences identity, accent,
and style but cannot make the English checkpoint synthesize another language.

## 7. Audio and synthesis

The provider advertises:

- complete `audio/wav;codec=pcm_s16le`;
- incremental `audio/pcm;codec=pcm_s16le`.

Both carry mono signed 16-bit little-endian samples at 24,000 Hz. Complete WAV
adds the canonical 44-byte header. Incremental delivery emits
`synthesis.audio_begin`, one or more bounded audio frames, then the request
terminal response.

One initialized process owns one warm model and prepared voice state. Requests
are sequential per UtterPipe session. Text is bounded by host limits and the
provider's tokenizer/runtime checks. Long text is split at sentence boundaries;
chunk seeds derive deterministically from the request seed and chunk index.
Output byte count, frame count, and SHA-256 are tracked through generation.

Cancellation is cooperative inside XN generation. The cancel response is the
wire linearization point: after an accepted acknowledgement no further audio
frames may be emitted, and the original synthesis terminates with stable code
`cancelled`. Deadline, output bound, closed backpressure channel, and native
failure produce stable terminal failures without leaking user text or model
internals.

## 8. Storage and transactions

Direct commands use platform-standard UtterPipe roots unless individually
overridden. Protocol sessions require the host to supply absolute distinct data
and cache roots; protocol mode performs no environment discovery.

The data layout is conceptually:

```text
data/
  schema.json
  mutation.lock
  models/pocket-tts-english-2026-04-q8/
    active
    versions/<bundle-revision>/
      manifest.json
      installation.json
      config.json
      model.gguf
      tokenizer.model | tokenizer.json
      lease.lock
  voices/<voice-id>/
    active
    versions/<normalized-sample-hash>/
      reference.wav
      metadata.json
      lease.lock
  voice-states/pocket-tts-english-2026-04-q8/<voice-id>/
    active
    versions/<derived-revision>/
      voice.safetensors
      manifest.json
      lease.lock
cache/
  downloads/sha256/<source-content-hash>
```

All mutations acquire one nonblocking cross-process mutation lease. New
artifacts are written to private unpredictable staging paths, synced, verified,
renamed into immutable versions, and finally activated through an atomic
pointer. Failures and cancellation leave the previous active version intact.

Runtime processes hold shared model, voice, and voice-state leases. Removal
must acquire exclusive leases for every requested artifact before changing any
active pointer; otherwise it returns `resource_busy`. Crash-released OS locks
and immutable versions make interrupted cleanup recoverable.

## 9. Management behavior

The provider implements the UtterPipe v1 management surface for:

- validation and actionable incomplete-state reporting;
- model and voice catalogs;
- plan/apply preparation with explicit disclosure acceptance;
- local voice import with explicit consent;
- exact artifact removal.

Plans bind the requested selection and observed artifact state. Apply rejects a
stale plan instead of silently changing scope. EOF/shutdown cancels active
management work, waits for its bounded grace behavior, and never publishes a
partial result. Human CLI commands call the same store operations.

Stable public failures include invalid options, missing model/voice,
authentication required, disclosure/consent required, integrity error,
resource busy, cancellation, timeout, and generic unavailable/internal errors.
Credentials, source URLs containing user information, provider stderr, model
tensors, and synthesis text are not placed in public diagnostics.

## 10. Release and verification

Supported release targets are:

- `aarch64-apple-darwin`;
- `x86_64-apple-darwin`;
- `x86_64-unknown-linux-gnu`;
- `x86_64-pc-windows-msvc`.

Release archives contain the executable, documentation, license report, and
installers. They never contain a model, source cache, imported voice, or local
benchmark sample. Each archive has a SHA-256 companion; source and CycloneDX
SBOM assets are published separately. Current signing limitations are recorded
in [release-integrity.md](release-integrity.md).

Required local/release verification includes formatting, strict all-target
Clippy, all ordinary tests, release build, provider diagnostic smoke, bundle
hash checks, real XN synthesis/cancellation with authorized fixtures, UtterPipe
conformance, benchmark resource evidence, and retained acoustic review. Model
preparation must reproduce the exact Q8 hash on every supported target before a
release can claim cross-platform bootstrap support.

## 11. Deferred work

Deferred candidates include additional fully compatible model/language
profiles, GPU backends where cross-platform packaging and measured benefit
justify them, richer model/voice management UI, more automated acoustic anomaly
detection, and optional quality/speed profiles. None should broaden the default
catalog until a first-run path is deterministic and its license, compatibility,
resource, protocol, and listener evidence is retained.
