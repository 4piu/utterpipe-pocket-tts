# Pocket TTS provider specification

Status: implemented
Provider slug: `pocket-tts`
Executable: `utterpipe-pocket-tts`
Provider version: `0.1.0`
UtterPipe protocol majors: `1`

This document is normative for this provider. It supplements the host-neutral
[UtterPipe Protocol v1](https://github.com/4piu/utterpipe/blob/main/docs/SPEC.md)
specification and does not change that contract.

## 1. Purpose

`utterpipe-pocket-tts` exposes Pocket TTS as a local, CPU-oriented, offline
UtterPipe provider. The provider is one self-contained native executable; model
weights and user-approved reference voices remain separate provider-managed
assets.

The provider uses Pocket TTS through sherpa-onnx rather than embedding Python.
It manages the exact converted model artifact it supports, imports reference
WAV files as named voices, keeps a warm inference engine for a runtime session,
and emits real incremental PCM while inference is running.

## 2. Goals and non-goals

Goals:

- one native provider file for each supported desktop target;
- no Python, package environment, daemon, GPU, or network during runtime;
- explicit, checksum-verified model preparation with license disclosure;
- explicit import of a reference voice for which the user confirms consent;
- mandatory complete WAV and preferred incremental PCM delivery;
- cooperative engine-side cancellation;
- safe independent processes sharing immutable model and voice assets;
- deterministic defaults and bounded CPU, memory, text, and audio.

Non-goals:

- bundling model weights or voice recordings in the provider executable;
- silently downloading a model during initialization or first synthesis;
- live discovery or automatic trust of the complete upstream voice repository;
- training, fine-tuning, recording, microphone capture, or voice cleanup;
- SSML, expression tags, GPU inference, or remote inference;
- accepting arbitrary ONNX graphs or model directories;
- claiming that a user-supplied recording is lawful or consented beyond the
  explicit confirmation captured during import.

## 3. Upstream and licenses

The engine integration is pinned to:

- Pocket TTS: <https://github.com/kyutai-labs/pocket-tts>;
- official model card and use conditions:
  <https://huggingface.co/kyutai/pocket-tts>;
- optional upstream reference recordings:
  <https://huggingface.co/kyutai/tts-voices> (licenses and attribution vary by
  collection; the provider curates only explicitly pinned entries);
- sherpa-onnx source tag `v1.13.4`, commit
  `142807252687d81b40d6315f23470a1512a00de3`:
  <https://github.com/k2-fsa/sherpa-onnx>;
- sherpa-onnx's pinned C API and static native libraries, accessed through the
  repository's narrow Pocket-only Rust binding;
- converted model artifact
  `sherpa-onnx-pocket-tts-int8-2026-01-26.tar.bz2` from the sherpa-onnx
  `tts-models` release.

The provider adapter and sherpa-onnx are Apache-2.0. The upstream multi-engine
static bundle includes an eSpeak NG archive, but Pocket does not use that
frontend. Release builds physically exclude the GPL archive and resolve the
generic factory's three retained eSpeak symbols with fail-closed Apache-2.0
shims; real-model release tests run with the archive absent. These engine and
adapter licenses do not determine the model or voice licenses.

The converted archive contains a CC-BY-4.0 license, attributes the conversion
to `KevinAHM/pocket-tts-onnx-export`, points to the upstream Pocket TTS terms,
and also states that the artifact is for non-commercial use. The provider
therefore presents all of the following as separately required acknowledgments
and applies the most restrictive disclosed interpretation:

1. `pocket-tts-cc-by-4.0` — attribution terms;
2. `pocket-tts-acceptable-use` — the upstream model-card conditions, including
   the prohibition on non-consensual cloning and deceptive impersonation;
3. `pocket-tts-converted-artifact-non-commercial` — the archive's explicit
   non-commercial notice.

This is a conservative product policy, not a legal conclusion. Preparation
must not proceed unless the user explicitly accepts all three disclosures. A
future model revision may remove or change this restriction only after its
artifact provenance and terms are independently verified and assigned a new
model ID.

The provider bundles no voice audio. Its direct CLI can fetch explicitly pinned
CC0 Voice-Zero entries from the official repository after showing their source,
license, and attribution. An imported reference remains the user's asset. Its
operational metadata records consent confirmation and content hash, but the
provider does not grant rights to the recording or generated voice.

## 4. Supported artifacts

| OS | Architecture | Target | Runtime prerequisites | Verification status |
| --- | --- | --- | --- | --- |
| macOS | arm64 | `aarch64-apple-darwin` | macOS 11+ | verified locally |
| macOS | x86_64 | `x86_64-apple-darwin` | macOS 11+ | planned |
| Windows | x86_64 | `x86_64-pc-windows-msvc` | Windows system DLLs; static MSVC CRT | verified locally |
| Linux | x86_64 | `x86_64-unknown-linux-gnu` | compatible glibc and libstdc++ | verified locally |

Only targets that pass the full native inference and conformance matrix in
release CI may be published. Local release probes produced a 24 MiB macOS
arm64 Mach-O, a 35,574,584-byte Linux x86_64 ELF, and a 24,285,184-byte Windows
x86_64 PE. The macOS and Windows binaries depend only on OS libraries; the
Linux binary additionally uses the system glibc, libstdc++, libgcc, and libm.

“Single executable” describes the provider program, not its roughly 98 MB
compressed model download, roughly 198 MB selected installed model files, or
imported voice data.

## 5. Technology choice and verified spike

Implementation language: Rust 2024 edition, minimum Rust 1.88.

The provider calls the pinned sherpa-onnx C API through a narrow local safe
binding and statically links its Pocket dependencies and ONNX Runtime archives.
It deliberately excludes the unrelated eSpeak NG and Unicode-data archives.
Release builds must never fetch native libraries implicitly: CI downloads the
exact per-target archive, verifies a repository-pinned SHA-256 value, removes
the excluded archive before Cargo runs, and supplies the remaining library
directory through `SHERPA_ONNX_LIB_DIR`. Local builds may instead provide the
verified original archive through `SHERPA_ONNX_ARCHIVE_DIR`.

The verified `v1.13.4` static native archive SHA-256 values are:

| Target | SHA-256 |
| --- | --- |
| macOS arm64 | `57801db2bbb786a5d343f515a38ff210b401842338bdc804fa075312d1cd2404` |
| macOS x86_64 | `2bda2c10b31a1cfc45d9f9e14bd4983743ec3779d309e42d99a6c8fa1689043f` |
| Linux x86_64 | `98b0e31996426f6e78244dbce1955548f2c64e8f01c4be75b85af7cdaa2e8d5c` |
| Windows x86_64 MT release | `d81bd1d25112540862d2387072e76b2b6843ef962918d6b5c7db5a19c6276b4c` |

The 2026-08-06 feasibility probe established:

- static linking and single-file packaging on Apple Silicon;
- 24,000 Hz mono output;
- 19.367 seconds of speech generated in 3.020 seconds with two inference
  threads (real-time factor 0.156 on the test machine);
- the first 28,800-sample callback after about 0.65 seconds, followed by 20
  consecutive callbacks whose sample counts equal the final waveform;
- returning false from the first callback stopped inference after about 0.64
  seconds and returned only that 1.2-second partial waveform.

The same day, Linux x86_64 and Windows x86_64 passed the complete mock and
process test suites plus the opt-in real-model test. That test exercised
incremental multi-frame output, bounded output rejection, cancellation after a
genuine callback, warm-engine reuse, and cross-process asset leases. Windows
uses a static MSVC CRT to match sherpa-onnx's pinned `MT` archive.

The callback is therefore genuine incremental generation and cooperative
cancellation, not post-generation slicing. Performance figures are evidence,
not guaranteed product limits.

Rejected alternatives for version 0.1:

- the upstream Python package would prevent a self-contained provider binary;
- invoking a Python subprocess would complicate installation, cancellation,
  framing, and process cleanup;
- accepting arbitrary ONNX exports would make compatibility and integrity
  untestable;
- bundling weights or sample voices would enlarge the binary and entangle
  distribution licenses.

## 6. Identity and capabilities

The hello response identifies:

```json
{
  "slug": "pocket-tts",
  "name": "Pocket TTS provider",
  "vendor": "UtterPipe contributors",
  "version": "0.1.0"
}
```

Capabilities and formats:

```json
{
  "capabilities": [
    "synthesis", "synthesis.cancel", "catalog",
    "prepare", "remove", "asset.import"
  ],
  "audio_deliveries": [
    {"mode":"complete", "format":"audio/wav;codec=pcm_s16le"},
    {"mode":"incremental", "format":"audio/pcm;codec=pcm_s16le"}
  ]
}
```

## 7. Provider options

Unknown options are rejected. Runtime initialization requires `model` and
`voice`; management initialization accepts a partial object, including `{}`.

| Option | Type | Default | Rules and meaning |
| --- | --- | --- | --- |
| `model` | string | required | released sherpa ID or installed experimental `pocket-tts-english-2026-04-q8` |
| `voice` | string | required | installed voice ID matching the import rules |
| `num_threads` | integer | backend/platform default | `1..64`; CPU inference threads |
| `max_reference_audio_seconds` | number | `10.0` | finite `1.0..30.0`; sherpa only |
| `voice_embedding_cache_capacity` | integer | `16` | `1..128`; sherpa only |

The provider publishes complete and partial JSON Schema Draft 2020-12 schemas
with `additionalProperties: false`. None is a secret. There is no configurable output
sample rate: the provider reports the engine's actual 24,000 Hz output and the
host adapts it to the playback device.

The released sherpa resolved utterance-options schema exposes `speed` (finite
`0.5..2.0`, default `1.0`) and `seed` (`0..4294967295`, default `42`). The XN
schema exposes only `seed`; any supplied speed field is rejected because the
runtime has no speed control. The host sends a value only for the current
request; all provider options are startup-fixed.
`seed` makes repeated requests reproducible only to the extent supported by the
pinned engine, target, and thread configuration. Bit-identical output across
platforms is not promised.

## 8. Model catalog and integrity

Version 0.1 knows exactly one model:

```text
id: pocket-tts-int8-2026-01-26
name: Pocket TTS int8 (sherpa-onnx conversion, 2026-01-26)
languages: [en]
download bytes: 98336520
selected installed file bytes: 198310873
archive sha256: 2f3b88823cbbb9bf0b2477ec8ae7b3fec417b3a87b6bb5f256dba66f2ad967cb
```

Stable source disclosure:

```text
https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/sherpa-onnx-pocket-tts-int8-2026-01-26.tar.bz2
```

The provider installs only the seven required graph/data files plus the
archive's README and license. It does not install or expose the archive's test
WAV files as voices. Each installed file is checked against this manifest:

| File | SHA-256 |
| --- | --- |
| `decoder.int8.onnx` | `12b0857402d31aead94df19d6783b4350d1f740e811f3a3202c70ad89ae11eea` |
| `encoder.onnx` | `e8f2f6d301ffb96e398b138a7dc6d3038622d236044636b73d920bab85890260` |
| `lm_flow.int8.onnx` | `8d627d235c44a597da908e1085ebe241cbbe358964c502c5a5063d18851a5529` |
| `lm_main.int8.onnx` | `bfc0c7e7e3d72864fa3bb2ee499f62f21ddc1474b885f5f3ca570f8be73e787e` |
| `text_conditioner.onnx` | `0b84e837d7bfaf2c896627b03e3f080320309f37f4fc7df7698c644f7ba5e6b1` |
| `token_scores.json` | `5be2f278caf9b9800741f0fd82bff677f4943ec764c356f907213434b622d958` |
| `vocab.json` | `6fb646346cf931016f70c4921aab0900ce7a304b893cb02135c74e294abfea01` |
| `README.md` | `2d05e627fe4fa3c625e822efcd20ad2ca62eb3b4fc1d67ae2625cb106b4f689c` |
| `LICENSE` | `fe7b4ce83b8381cc5b216bbb4af73c570688d1b819c73bbaed8ca401f4677cd6` |

The catalog reports `available`, `installed`, `incomplete`, or `incompatible`.
Upstream mutation at the same URL is an integrity error, never an implicit
upgrade. A replacement requires a new model ID and manifest.

### Experimental XN bundle boundary

Development builds recognize `pocket-tts-english-2026-04-q8` only through an
explicit local bundle import. It is not advertised as remotely available. The
bundle directory contains the three runtime payloads `model.gguf`,
`config.json`, and `tokenizer.json`, plus `manifest.json`. Manifest schema
`utterpipe.pocket-tts.xn-bundle/1` binds:

- model ID, display name, version, English language, source repository, and
  immutable source revision;
- XN engine ID, project-fork revision, XN version, Q8_0 precision, and April
  compatibility class;
- temperature, fixed streaming-safe output gain, and text/post-EOS behavior for
  that model generation;
- every disclosure ID, name, and HTTPS URL; and
- exact byte length and lowercase SHA-256 for all three runtime payloads.

Unknown fields, relative or symlink roots, non-regular payloads, duplicate or
invalid disclosures, unsupported runtime identifiers, unsafe behavior values,
oversized files, malformed tokenizer/config data, sample rates other than 24
kHz, and any size/hash mismatch fail closed. A canonicalized-manifest SHA-256
is the immutable installed revision. Installation copies into version staging,
verifies the copy, loads the full Mimi voice encoder, records accepted
disclosures separately, syncs, and only then atomically publishes `active`.

The full Q8 GGUF retains the voice encoder. Importing a normal validated WAV
creates a compact safetensors state under a model- and reference-specific
revision. The provider applies upstream PCM scaling, loudness normalization,
24 kHz resampling, and prompt trimming once during preparation. Runtime
initialization verifies and leases the model, original reference, and derived
state together. Removing the reference also removes its derived state after
proving that no runtime holds the reference lease.

## 9. Voice import and catalog

The provider embeds no voice recordings. Its direct CLI contains an offline
manifest of sixteen English-compatible recordings from four collections in
`kyutai/tts-voices`, pinned to repository revision
`323332d33f997de8394f24a193e1a76df720e01a` with exact byte sizes and SHA-256
digests. Voice-Zero is CC0 1.0; Alba MacKenna and VCTK are CC BY 4.0;
Expresso is non-commercial CC BY-NC 4.0. Each manifest entry carries its exact
collection attribution. `voices available` reads only this local manifest.
Interactive terminals show a numbered eight-item pager; redirected output is
JSON Lines with the same stable one-based numbers. `voices install` with no
selection opens the pager and multi-select prompt. Explicit arguments may mix
IDs, numbers, comma/space lists, and ascending numeric ranges. All selected
licenses are shown and accepted before any download. Immutable sources are
all downloaded and verified before the first import, then passed sequentially
through the ordinary importer. Each installed voice is an independent atomic
store mutation; an earlier completed import remains installed if a later import
conflicts or fails. `--id` is restricted to a single selection. The complete
live repository is not enumerated or treated as uniformly licensed.

| # | Catalog ID | License | Repository path | Bytes | SHA-256 |
| ---: | --- | --- | --- | ---: | --- |
| 1 | `voice-zero-bill-boerst` | CC0 | `voice-zero/bill_boerst.wav` | 955496 | `be4815e4fb760ba1b78117545a260cce4a4c124c7657bc5c6127a0fef8ba661f` |
| 2 | `voice-zero-caro-davy` | CC0 | `voice-zero/caro_davy.wav` | 743528 | `40c692c005a0268a7a5b6ebae348077d3dca6a86eb6b12bd36e343bbcd71b5f6` |
| 3 | `voice-zero-peter-yearsley` | CC0 | `voice-zero/peter_yearsley.wav` | 524448 | `fbb3920fda7ae26a5a8b317ffcae1d55c0bd5d89d075205f5a52b1e924b83f51` |
| 4 | `voice-zero-stuart-bell` | CC0 | `voice-zero/stuart_bell.wav` | 745776 | `00c7baeb2fb7a8c1c6198e045b5e853a7ccc04002a51a09b4be3dd7c96994f73` |
| 5 | `alba-mackenna-a-moment-by` | CC BY | `alba-mackenna/a-moment-by.wav` | 958542 | `a1805f0e3610f0d5985f4abb51979620a012899e810019960310944bbcba509d` |
| 6 | `alba-mackenna-announcer` | CC BY | `alba-mackenna/announcer.wav` | 958542 | `e8b55193435db043833dda62fb759ee2779ace195811340ee8d28c7c4a4ccc24` |
| 7 | `alba-mackenna-casual` | CC BY | `alba-mackenna/casual.wav` | 958542 | `46264e83cb99115c3d210260e029117566d9c64f20266d10daa78107759ede3e` |
| 8 | `alba-mackenna-merchant` | CC BY | `alba-mackenna/merchant.wav` | 966734 | `52c24756de299b37998ed83e32fdc8747f874f9dd67f0bcdc38b96d3f70cf488` |
| 9 | `vctk-p225` | CC BY | `vctk/p225_023.wav` | 1166878 | `4f15f804be0f437912697ffaa56b03759e10b5e1db82fcdac20412fe95bedec9` |
| 10 | `vctk-p226` | CC BY | `vctk/p226_023.wav` | 1166730 | `80b7c8d8eb9129af901750897727647291e13418dab919e3922ba58b482cf9a9` |
| 11 | `vctk-p227` | CC BY | `vctk/p227_023.wav` | 1217202 | `ee47295e38d1814446c8819364e100c12208c36e267aa216feabe8884eb8ada7` |
| 12 | `vctk-p228` | CC BY | `vctk/p228_023.wav` | 1206922 | `675eccc60019e09cb0e0f5bfaa2364f6406ce3eb520a776811bb3513358ad5a8` |
| 13 | `expresso-ex01-default` | CC BY-NC | `expresso/ex01-ex02_default_001_channel1_168s.wav` | 960044 | `7e196b0f345e11f4d54fbcf4376b3f1f845837f5122f7dd2e1c040410ec3c3c8` |
| 14 | `expresso-ex01-enunciated` | CC BY-NC | `expresso/ex01-ex02_enunciated_001_channel1_432s.wav` | 960044 | `e97124f3cd441dcb762e9900f7e6432b342efcfa1dd404c49d8fb80b6e0fa70d` |
| 15 | `expresso-ex01-fast` | CC BY-NC | `expresso/ex01-ex02_fast_001_channel1_104s.wav` | 960044 | `a6e52ea63a1b4b51b66ddad62c40af18a9f510baeea250bad52b631b7edeb95f` |
| 16 | `expresso-ex01-whisper` | CC BY-NC | `expresso/ex01-ex02_whisper_001_channel1_579s.wav` | 960044 | `292ee886268549c3a059fed12e39c07fcd90229ecb59abd25da6ecf986a7a882` |

The generic `voices` catalog returned by `catalog.items` lists provider-owned
installed voices compatible with the model and returns `{"voice":"<id>"}`
selection patches. Protocol `asset.import` remains local-file-only.

`asset.import` with kind `voice` requires:

- an absolute `source_path`;
- a `requested_id` matching `[a-z0-9][a-z0-9._-]{0,62}[a-z0-9]`, or one
  lowercase alphanumeric character;
- `consent_confirmed: true`;
- a RIFF/WAVE, mono, uncompressed PCM16 reference at 16,000–48,000 Hz;
- duration from 1.0 through 30.0 seconds;
- a regular classic RIFF/WAVE file whose declared 32-bit RIFF size matches the
  source.

The importer opens without following a final symlink where the platform allows,
validates checked chunk sizes, streams and hashes the complete container, skips
metadata with a fixed buffer, and allocates only the format- and
duration-validated PCM payload. It checks cancellation between chunks, verifies
that the data did not change between inspection and decoding, and writes a
normalized provider-owned mono PCM16 WAV. A source larger than 5 MiB is warned
about by the direct CLI but is not rejected. Version 0.1 preserves the input
sample rate because sherpa-onnx accepts the reference rate explicitly.

Metadata stores the voice ID, SHA-256 of decoded sample content and source file,
sample rate, sample count, import time, model compatibility, and the boolean
consent confirmation. It never stores the original path or arbitrary URL.
Curated imports additionally store their public repository, revision, path,
license, and attribution. Reimporting identical content to the same ID is
idempotent; different content at an existing ID is an explicit conflict and
never overwrites silently.

The catalog reports ordinary references as kind `imported` with license ID
`user-provided-reference`. Verified manifest downloads are kind `curated` and
retain their collection-specific source and license disclosure. The provider
makes no identity inference from audio.

## 10. Storage and concurrency

The operational schema version is 1:

```text
data_dir/
  schema.json
  mutation.lock
  models/pocket-tts-int8-2026-01-26/
    active
    versions/<archive-sha256>/
      lease.lock
      manifest.json
      LICENSE
      README.md
      *.onnx
      *.json
  voices/<voice-id>/
    active
    versions/<sample-sha256>/
      lease.lock
      metadata.json
      reference.wav
cache_dir/
  downloads/sha256/<archive-sha256>
  tmp/<random-operation-id>/
```

Active version directories are immutable. `active` is a small, validated
version token published with platform-native atomic replacement after the new
directory and metadata are synced. Runtime initialization reads each pointer,
opens its `lease.lock`, and holds a shared OS advisory lock for the process
lifetime. Removal obtains an exclusive lease lock; a live runtime therefore
causes immediate `resource_busy`. OS lock release provides crash-stale recovery;
stale lock files are harmless.

Every mutation first takes `mutation.lock` exclusively without waiting. A
second management process returns `resource_busy`. Preparation uses a
process-unique cache temporary, enforces compressed and extracted byte/count
limits, rejects absolute paths, traversal, symlinks, hard links, devices, and
unexpected required-file duplicates, then renames a complete content-addressed
directory into place. Concurrent cache publication accepts an already-present
file only after verifying its hash.

A runtime never takes the mutation lock for its lifetime and never writes data.
Two or more runtimes may hold shared leases and read the same asset. A runtime
starting during activation sees either the complete old pointer or complete new
pointer. Model and voice versions can coexist until unleased cleanup.

Version 0.1 accepts only operational schema 1. Unknown schema versions fail
with `engine_unavailable` and do not mutate.

## 11. Preparation and removal

`prepare.plan` for a missing selected model returns one download/extract action,
the exact sizes, source, archive hash, and all three required disclosures from
section 3. It does not plan or invent a voice. Its summary tells the user to run
`voices import` separately when the selected voice is absent.

`prepare.apply` requires the same live plan and all listed acceptance IDs. It
may connect only to the pinned HTTPS model source, follows at most five HTTPS
redirects limited to GitHub's release-asset hosts, disables proxies, applies a
30-second connect timeout and a 10-minute total timeout, streams no more than
the declared compressed size plus a small HTTP framing tolerance, verifies the
archive before extraction, verifies each selected file, and activates only the
complete version.

An existing valid version makes apply idempotent. An interrupted preparation
leaves temporary state in cache only and the old active version usable.

`remove.plan` accepts exact logical artifact IDs (`model:<id>`, `voice:<id>`)
or an explicit purge flag and reports affected bytes. `remove.apply` revalidates
the plan, refuses leased assets without partial removal, confines deletion to
validated direct children of the provider roots, and never follows symlinks.
Removing cached downloads is separate from removing the active installed model.

## 12. Initialization

Inspect hello is side-effect-free. A management session validates options and
may initialize with a missing selected model or voice for catalog, prepare, or
import operations.

A runtime session reads the model and voice exclusively from `provider_options`,
validates schema, model manifest, voice metadata and WAV,
takes shared leases, constructs `OfflineTts` from the pinned active graph files,
and loads the reference samples. It must not download, migrate, write a cache,
or synthesize during initialization. Missing assets map to `resource_missing`;
corrupt assets map to `integrity_error`; engine construction
failure maps to `engine_unavailable`.

One warm engine instance remains alive for repeated sequential synthesis until
shutdown or EOF. It has no idle timeout. Each provider OS process owns its own
engine, threads, embedding cache, protocol session, and memory; shared state is
limited to immutable files and locks.

## 13. Synthesis

The provider validates the negotiated text limit, rejects empty text and NUL,
and otherwise passes the Unicode string unchanged to the engine. It does not
interpret XML, SSML, markdown, or expression tags and does not log the text.

Generation uses the initialized model, reference voice, effective `speed`, `seed`, and
`max_reference_audio_seconds`. Output is mono 24,000 Hz float PCM from the
engine. XN bundles declare a fixed `output_gain` in `(0, 1]`, applied to every
decoded sample before conversion; the verified April bundle uses `0.9` to
provide streaming-safe headroom without whole-utterance peak normalization or
limiting. Samples are then converted to little-endian PCM16. Values are clamped
to `[-1.0, 1.0]`; non-finite samples fail synthesis; negative full scale maps
to `-32768` and positive full scale to `32767`. The provider applies no
playback-device gain and performs no device-rate resampling. Complete and
incremental paths use the same scaled PCM chunks.

For incremental delivery, the engine runs on a dedicated blocking thread. Each
callback contains newly generated consecutive samples. It converts and submits
them to a bounded writer channel immediately; channel backpressure may pause the
engine callback but cannot block the protocol reader. The provider emits
`synthesis.audio_begin` immediately before the first audio frame and reports
exact cumulative byte and frame counts in the terminal response. It does not
add an artificial prebuffer.

For complete delivery, the same callback converts into a bounded in-memory PCM
buffer. On success the provider creates one internally consistent PCM16 RIFF/WAV
and emits it according to the complete-delivery contract.

The callback checks the cancellation flag, negotiated deadline, and cumulative
audio bound before publishing each chunk. Exceeding time or size stops engine
generation and returns `timeout` or `output_too_large`. Under incremental
delivery a late failure is terminal after partial audio and is never retried.
The internal sherpa silence scale is fixed to `1.0`: Pocket otherwise performs
a whole-output silence rewrite after its callbacks, which would make genuine
incremental chunks differ from the final native buffer. This setting makes the
callback concatenation and complete output sample-for-sample identical.

Only one synthesis runs at a time. Other synthesis and health requests receive
`busy`; cancellation and shutdown remain responsive. Engine panics or invalid
samples become `synthesis_failed` without process memory details.

## 14. Cancellation and shutdown

The protocol reader, protocol writer, and blocking inference worker have
separate execution paths. `synthesis.cancel` atomically marks the active request
and returns accepted promptly. The callback observes the flag and returns false;
the writer discards queued chunks after cancellation and the original synthesis
terminates with `cancelled`. The verified engine callback makes this genuine
engine-side cancellation.

Cancellation grace is two seconds. If inference does not return, the provider
exits nonzero so the host can replace it. Shutdown uses the same cancellation,
joins the worker within the grace period, releases voice/model leases, returns
the shutdown response, and exits. EOF performs equivalent best-effort cleanup.
The provider creates no child process.

## 15. Network and privacy

Hello, initialization, validation, catalogs without refresh, runtime health,
protocol asset import, and all synthesis are network-free. The engine must be
configured from explicit local paths so it cannot auto-download.

Only explicit model preparation, direct `voices install`, or direct
`voices import <HTTP(S)-URL>` may use the network. Download clients honor system
proxy configuration and standard proxy environment variables. URLs use normal
HTTP client semantics, including embedded credentials when supplied by the
human caller. Runtime speech remains offline: spoken text, local reference
samples, and generated audio are not uploaded. Diagnostics contain stable IDs
and error classes rather than text, sample content, or stack traces.

## 16. Direct CLI

The executable provides:

```text
utterpipe-pocket-tts info
utterpipe-pocket-tts doctor [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts models list [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts models prepare [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts models remove <id> [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts voices list [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts voices available [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts voices install [<id-or-number-or-range>...] [--id <id>] \
  [--accept <license-id>]... [--yes] [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts voices import <path-or-http-url> --id <id> --consent-confirmed \
  [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts voices remove <id> [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts protocol --stdio
```

For these direct human commands only, an omitted root resolves to the current
user's platform-standard Pocket provider location. macOS uses
`~/Library/Application Support/UtterPipe/providers/pocket-tts/data` and
`~/Library/Caches/UtterPipe/providers/pocket-tts`; Linux uses
`${XDG_DATA_HOME:-~/.local/share}/utterpipe/providers/pocket-tts` and
`${XDG_CACHE_HOME:-~/.cache}/utterpipe/providers/pocket-tts`; Windows uses the
`data` and `cache` children of
`%LOCALAPPDATA%\UtterPipe\providers\pocket-tts`. Each explicit flag overrides
only its corresponding default. A missing, empty, relative, or otherwise
unusable platform base fails closed and tells the user to pass that root.

This discovery never applies to `protocol --stdio`. Under the provider
protocol, the host remains solely responsible for supplying the absolute,
private, distinct storage roots in `session.initialize`; the provider neither
falls back to direct-CLI defaults nor merges them with host values.

Mutating human commands print an equivalent human-readable plan. When stdin,
stdout, and stderr are all terminals, missing confirmation, disclosure
acceptance, and voice-consent flags are requested interactively after the plan
and disclosures are shown; each question defaults to refusal. If any of those
streams is redirected or piped, every required flag remains mandatory and
input is never consumed as authorization. Explicit flags bypass prompting.

The framed protocol never prompts and continues to require its explicit
plan/apply, disclosure-acceptance, and consent fields. Protocol plan IDs are
deliberately not reused: they belong to one live management session. The CLI
and protocol call the same validation, catalog, checked store, import,
download, preparation, and removal primitives, including the same
cross-process mutation and asset locks.

## 17. Errors and diagnostics

| Condition | Protocol error |
| --- | --- |
| Unknown/range-invalid fixed option | `invalid_provider_options` |
| Unknown/range-invalid per-request option | `invalid_utterance_options` |
| Unknown model or voice ID syntax | `invalid_provider_options` |
| Invalid reference-audio request or format | `invalid_message` |
| Selected model or voice absent | `resource_missing` |
| Model/archive/file hash mismatch | `integrity_error` |
| Required disclosure not accepted | `license_required` |
| Mutation or leased removal conflict | `resource_busy` |
| Engine construction/native failure | `engine_unavailable` |
| Empty, NUL, or excessive text | `invalid_text` |
| Deadline exceeded | `timeout` |
| Cumulative output over negotiated bound | `output_too_large` |
| Engine generation/invalid sample failure | `synthesis_failed` |
| Explicit cancellation | `cancelled` |

stderr is human-readable and protocol-safe. Default diagnostics include the
provider version, operation class, stable selected IDs, and error code. They
exclude spoken text, reference content, original import source path, and full
provider roots. Debug logging is a direct CLI concern and remains redacted.

## 18. Packaging and release

Each target archive contains the executable, Apache-2.0 license, third-party
notices, and build documentation. Its SHA-256 checksum, corresponding source,
and CycloneDX SBOM are separate release assets. Model and voice artifacts are
never inside the release executable or archive.

The lockfile pins all Rust dependencies. Native sherpa archives are pinned by
version and checksum in release configuration, fetched before Cargo runs, and
made available through an explicit local-archive or local-library setting. CI
removes the unused GPL archive and rejects its absence before that exclusion,
so an upstream layout change fails closed. Published targets run clean-machine
inference, protocol conformance, license-display, and dynamic-dependency checks.

Provider binary upgrades preserve operational schema 1. Model upgrades use a
new model ID/version directory and atomic activation; they never rewrite an
immutable active graph in place.

## 19. Acceptance tests

In addition to the common UtterPipe conformance suite, release acceptance
requires:

- exact pinned model archive and per-file integrity checks;
- archive traversal, link, duplicate, oversized-entry, and decompression-bomb
  rejection;
- all three model disclosures shown and required before network or mutation;
- offline runtime verification by denying network access;
- valid and invalid PCM16 voice imports, duration/rate/RIFF limits, large
  streamed metadata and warning behavior, cancellation, consent refusal,
  idempotent import, collision refusal, and source-path redaction;
- offline curated catalog metadata; pinned network download size/hash/license/
  attribution verification; HTTP(S) import; proxy behavior; private staging and
  cleanup after success, failure, timeout, or cancellation;
- a real Pocket golden synthesis with non-silent, bounded 24 kHz mono output;
- callback sample concatenation equal to complete generation output;
- first incremental frame observed before terminal completion;
- cancellation after the first callback and during backpressure;
- complete/incremental byte counts, PCM clipping, non-finite rejection, timeout,
  and maximum-output enforcement;
- repeated warm synthesis and prompt `busy` behavior;
- two runtime processes using the same model and voice;
- runtime during activation, two competing preparations, leased model/voice
  removal, crash-stale lease recovery, cache publication races, interrupted
  extraction, and independent roots;
- clean shutdown, EOF, forced termination, stderr flooding, and engine panic;
- per-target single-file dependency inspection; exact provider executable,
  companion-library, compressed-package, and installed-runtime sizes; plus
  cold/warm latency, real-time factor, peak/steady resident memory, accelerator
  memory where applicable, and persistent-cache growth;
- a short opt-in listening smoke test on each published platform.

Subjective voice quality is reviewed against an intelligibility and severe-
artifact floor, but maximum fidelity is not the primary pass criterion. Preserve
user-selectable speed, memory, and quality profiles where the chosen runtime can
offer them reliably.

## 20. Release gates

The implementation may proceed against this specification. Public release is
blocked until all of the following are true:

1. the provisional UtterPipe name is cleared;
2. every published native sherpa archive has a checked-in trusted checksum;
3. legal/provenance notices reproduce the converted model's exact terms and the
   conservative non-commercial policy in section 3;
4. the target passes the real inference, concurrency, and conformance matrix;
5. signing/notarization status is stated accurately for that artifact.

These are verification and distribution gates, not unresolved protocol or
implementation choices.
