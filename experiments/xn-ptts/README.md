# XN Pocket TTS runtime experiment

This isolated harness evaluates `ptts` 0.2.2 with `xn` 0.1.21 before any
production provider integration. It keeps one model and one prepared voice
state warm, resets the sampling seed for every run, pipelines autoregressive
generation into a bounded two-frame decoder queue, and reports first decoded
audio, completion, RTF, deterministic PCM identity, native peak RSS where the
OS exposes it, and after-first-audio cancellation behavior. Each measured run
also reports the decoded float minimum, maximum, absolute peak, and counts at or
outside `[-1, 1]` before PCM16 conversion so clipping can be distinguished from
integer endpoint rounding. Those diagnostics are always measured before
`--output-gain`; the saved WAV applies that fixed gain sample by sample. Use
`--output-gain 0.65` when reproducing the April provider profile.

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
  --output-gain 0.65 \
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

## Acoustic release corpus

`release-corpus.json` pins upstream's quantization-evaluation paragraph and
stress sentences, adds Agent Speak status/cancellation text, and expands them
over four pinned Voice-Zero references and seeds 7, 42, and 2026. The
`utterpipe-pocket-xn-release-corpus` binary accepts explicit authorized f32
assets, the installed Q8 provider store, and one prepared state for every plan
voice. It generates 84 cases: two Q8 WAVs through independently spawned
UtterPipe provider processes and one f32 control for each case.

The destination must be a new absolute path. Generation occurs in sibling
staging and publishes the directory only after every subprocess and report
contract succeeds. The directory contains 252 WAVs, their machine-readable run
reports, a strict `utterpipe.acoustic-manifest/1`, and pinned corpus provenance.
Run `utterpipe-acoustic-gate` on that manifest; the checked-in plan requires
zero clipping, a maximum peak fraction of 0.95, deterministic Q8 replay, and
the calibrated f32-relative overlay screen.

For semantic evidence, build pinned `whisper.cpp` v1.9.2 and supply its full
`small.en` model to `utterpipe-pocket-xn-asr-corpus`. The command reads the
existing release directory, transcribes the first Q8 replay and f32 baseline in
bounded batches, and writes a new manifest plus recognizer/model provenance;
it never overwrites the original manifest or audio. The checked-in plan pins
maximum f32-relative WER and CER deltas of 0.15 and 0.07. Those thresholds pass
the 84-case XN corpus and fail all six clips from the known-bad PocketTTS.cpp
INT8 positive control. Transcript text remains in the local input manifest and
is reduced to metrics and a reference hash by `utterpipe-acoustic-gate`.

Perceptual evidence and human review are separate release requirements from
the ASR result.

For perceptual evidence, use Python 3.10 or newer with PyTorch, torchaudio, and
librosa. Check out the plan-pinned `tarepan/SpeechMOS` revision, obtain its
plan-pinned `utmos22_strong` checkpoint, and run
`utterpipe-pocket-xn-perceptual-corpus` with the ASR-annotated manifest,
`score_utmos22.py`, the clean checkout, and checkpoint. The wrapper verifies the
Git revision, rejects any tracked or untracked checkout changes, checks the
model SHA-256, and requires exactly one finite candidate/baseline score for
every plan case. It writes another new manifest and provenance file without
overwriting its input.

UTMOS is accepted here because its `-0.75` minimum Q8-minus-f32 score policy
passes all 84 XN cases and rejects five of six known-bad PocketTTS.cpp INT8
positive controls; ASR rejects the remaining short clip. NISQA-TTS was also
probed but is not accepted because it scored several severely corrupted
positive controls above their clean fp32 baselines.

The five-pair retained review passed on 2026-08-12. Three Q8 candidates were
clean and two remained acceptable with only minor candidate-only defects; no
significant artifact or intelligibility failure was heard. The exact case IDs,
WAV hashes, ratings, and evidence hashes are recorded in
[`retained-review-2026-08-12.json`](retained-review-2026-08-12.json). The audio
corpus remains local and is not committed.
