# Pocket runtime evaluation

This record documents the evaluation that selected XN and replaced the former
sherpa-onnx backend. Selection required more than loading a newer model: the
runtime had to preserve cross-platform, offline, incremental, cancellable, and
single-executable behavior on Windows, macOS, and Linux. Historical sherpa and
rejected-candidate measurements remain below as comparison evidence.

## Current benchmark support

`utterpipe-conformance` verifies framing and synthesis. The separate
`utterpipe-benchmark` executable measures process startup, initialization,
incremental first audio, completion, output duration, RTF, bytes/frames,
cancellation, shutdown, sampled RSS, and artifact sizes through the same public
wire contract used by Agent Speak. Its `utterpipe.benchmark/1` JSON avoids
mixing model download or voice import into inference measurements. The ignored
provider real-model test additionally covers bundle installation, derived voice
state, output bounds, warm reuse, cancellation, and leases.

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

The same Pocket incremental workload was then built and exercised from isolated
checkouts on the Windows and Linux development hosts. Both used the identical
transferred model store and synthetic reference, two warmups, ten measurements,
two inference threads, and two after-first-audio cancellation attempts.

| Host | Initialize | First audio p50 / p95 | Completion p50 / p95 | RTF p50 / p95 | Steady / sampled-peak RSS | Executable |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Windows 11, Ryzen 7 9800X3D | 539 ms | 558 / 572 ms | 703 / 717 ms | 0.1373 / 0.1400 | 628.06 / 628.07 MB | 24.06 MB |
| Manjaro Linux, Core i9-12900K | 571 ms | 666 / 678 ms | 779 / 792 ms | 0.2212 / 0.2250 | 603.76 / 603.76 MB | 35.51 MB |

Both hosts completed real incremental synthesis and accepted both cancellation
requests without emitting audio after acknowledgement. Median acknowledgement /
terminal-cleanup latency was 0.101 / 46.9 ms on Windows and 0.179 / 58.2 ms on
Linux. Provider data remained 198.55 MB, the removable archive cache remained
98.34 MB, and neither store grew during measurement.

The seeded output was stable across all ten iterations on each host, but it was
not bit-identical across hosts. The same text and reference produced 5.20 seconds
on macOS, 5.12 seconds on Windows, and 3.52 seconds on Linux, with different PCM
hashes. Do not promise cross-platform acoustic reproducibility for the current
sherpa backend. The synthetic reference establishes runtime and protocol
behavior only; intelligibility and severe-artifact screening still require a
consented natural reference and listening evaluation.

The current eSpeak comparator was also rebuilt and measured from isolated
checkouts. A Windows verbatim-path compatibility fix was required before the
benchmark's canonical absolute cache roots worked with eSpeak's narrow CRT file
API.

| Host | Initialize | Completion p50 / p95 | RTF p50 / p95 | Steady / sampled-peak RSS | Executable |
| --- | ---: | ---: | ---: | ---: | ---: |
| Apple M4 macOS | 22 ms | 15.9 / 17.0 ms | 0.00505 / 0.00540 | 4.39 / 4.77 MB | 20.28 MB |
| Windows 11, Ryzen 7 9800X3D | 276 ms | 43.7 / 44.6 ms | 0.01396 / 0.01426 | 27.64 / 27.67 MB | 20.57 MB |
| Manjaro Linux, Core i9-12900K | 10 ms | 10.5 / 10.7 ms | 0.00336 / 0.00341 | 28.36 / 29.90 MB | 20.46 MB |

Windows and Linux produced the same deterministic 138,076-byte, 3.130-second
WAV. Each reconstructed eSpeak cache contained 18.46 MB across 515 files.

## Candidate set

| Runtime | Role in evaluation | Three-platform evidence | Deployment concern |
| --- | --- | --- | --- |
| Former sherpa-onnx 1.13.4 | Historical CPU baseline | Real incremental synthesis and cancellation passed on Windows, macOS, and Linux | Removed: high resident memory and incompatible model format |
| `xn-ptts` / XN | **Selected active runtime** | Provider conformance, benchmarks, cancellation, bootstrap, and acoustic gates pass across the three target platforms | Pinned project fork and one known-good Q8 catalog profile |
| `audio.cpp` Pocket implementation | Evaluated, not selected | macOS native build probe completed | Pocket path was offline-only and lacked cooperative cancellation; model packaging differed |
| `PocketTTS.cpp` | Rejected for this release | Native build and synthesis probes completed | Stock INT8 produced severe word-local artifacts; packaging and model-generation compatibility also needed upstream work |

Documentation alone was not admission evidence. Each viable candidate had to
build and synthesize on the development Mac, Windows 11 machine, and Manjaro
Linux machine. The official Python implementation served as an acoustic oracle,
not as a distributable provider backend.

The active catalog therefore includes only artifacts whose architecture,
tokenizer, voice state, license, integrity, output, and bootstrap flow were
verified with the selected pinned XN runtime.

### XN Pocket TTS probe

The first XN evaluation pins `xn-ptts` commit
`4398678425e1b3d48d525024257830aec989bc58`, its `ptts` 0.2.2 package, and
`xn` 0.1.21. The runtime loads Kyutai's official January 2026 safetensors
directly. Its native quantizer applies Q8_0 only to FlowLM transformer attention
and FFN projections; it does not use PocketTTS.cpp's rejected broad ONNX MatMul
quantization. The benchmark implementation and exact lockfile are under
[`experiments/xn-ptts`](../experiments/xn-ptts/README.md).

Adapter development subsequently moved to project fork commit
`52493c0fbc9f52916d93df2456fe75ea71da9a58`, based on that same upstream
revision. Its only engine change adds opt-in learned BOS-before-voice
conditioning while preserving the January path when the configuration field is
absent or false. The pre- and post-change January Q8 runs produced the identical
180,480-byte PCM SHA-256
`359243cf7a0d02984106f49b13002504b19747fe808403729f93b387972d3c9b`.
Fork commit `4dbd8d6832cf4e093d08a1bd4666a08783345e7b` then adds independently
configurable Mimi downsample and upsample dimensions. This is required by the
April 2026 English checkpoint, whose voice-encoding bottleneck is 32 channels
while its codec and decoder remain 512 channels wide. Omitted dimensions still
resolve to the legacy width.

The April probe pins official model revision
`19f95fe2df36e79fbd9f10008595cc4c977a0fcc`. Its 219,029,196-byte
`model.safetensors` has SHA-256
`473f47d99560bd50eb8b4509d3cacfe7f316ab20bdca86505403a2e6a936a6e9`.
The run uses the matching tokenizer, learned voice BOS, 32/512 Mimi bottleneck,
recommended temperature 0.3, no legacy short-input space padding, and the
current two-frame addition to the post-EOS heuristic. XN cannot consume the
official Python KV-cache voice safetensors directly, so it prepared its compact
434,296-byte state from the exact pinned CC0 Caro Davy WAV instead.

| April M4 profile | Threads | Load | Voice-state prompt | First audio p50 / p95 | Completion p50 / p95 | RTF p50 / p95 | Native peak RSS | Runtime model |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| XN Q8_0 | 2 | 65 ms | 70 ms | 25.2 / 26.2 ms | 432 / 478 ms | 0.1200 / 0.1329 | 244.56 MB | 128.74 MB |
| XN unquantized f32 compute | 2 | 427 ms | 115 ms | 48.4 / 50.5 ms | 623 / 638 ms | 0.1694 / 0.1734 | 664.73 MB | 219.03 MB |

Both profiles were deterministic across ten measured runs. Q8 generated 3.60
seconds and f32 generated 3.68 seconds from the same text, voice, and seed. Two
after-first-audio stops per profile retained exactly one 80 ms frame. The saved
matched listening pair remains a human acceptance gate before this model or its
Q8 conversion enters the quick-start catalog.

Listener acceptance on 2026-08-10 passed both April Q8 and f32 samples without
an audible quality or artifact failure. Q8 is therefore the leading
quick-start runtime profile for this model generation; provider-level protocol,
bundle, and cross-platform release gates still apply.

The provider-level XN adapter then passed those protocol and platform gates at
provider commit `d54f4d2be31d4e6930f3ea0ef332b16463bf5f30`, using
`utterpipe-benchmark` and `utterpipe-conformance` commit
`2bb8c478fe7457aa0b93075d225577cff65e921d`. Each host imported the same
manifest-verified full April Q8 bundle through the public direct CLI, imported
the pinned Caro Davy WAV, and prepared a model-specific voice state. Both
complete and incremental conformance passed before measurement. The benchmark
used seed 42, eight threads on x86, two warmups, ten measured utterances, and
two after-first-audio cancellation attempts. The macOS row uses the
architecture default of four threads. RSS is sampled direct-provider RSS and
its peak remains a lower bound.

| Host | Initialize | First audio p50 / p95 | Completion p50 / p95 | RTF p50 / p95 | Steady / sampled-peak RSS | Executable | Installed data |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Apple M4 macOS | 362 ms | 23.6 / 34.3 ms | 483 / 500 ms | 0.1059 / 0.1097 | 278.90 / 279.25 MB | 27.63 MB | 149.67 MB |
| Windows 11, Ryzen 7 9800X3D | 742 ms | 131.8 / 138.5 ms | 691 / 709 ms | 0.1516 / 0.1555 | 251.65 / 260.58 MB | 32.67 MB | 149.67 MB |
| Manjaro Linux, Core i9-12900K | 757 ms | 150.1 / 157.5 ms | 857 / 878 ms | 0.1847 / 0.1892 | 296.30 / 369.59 MB | 43.53 MB | 149.67 MB |

All six cancellation attempts were accepted, and no host emitted audio after
the cancellation acknowledgement. Median acknowledgement / terminal-cleanup
latency was 0.109 / 6.50 ms on macOS, 0.134 / 9.87 ms on Windows, and 0.155 /
10.62 ms on Linux. Stores did not grow during measurement and no cache was
created. The Windows executable imports only Windows system DLLs and no Visual
C++ runtime; the Linux executable imports only the platform's standard C,
math, GCC, and C++ runtime libraries.

Seeded PCM was stable across all ten measurements on each host but was not
byte-identical across operating systems. Windows and macOS produced 4.56
seconds while Linux produced 4.64 seconds. This is not a protocol defect, but
it means the release gate must retain platform-local acoustic and listening
coverage instead of treating one host's digest as universal. The complete
Windows and Linux versioned reports are retained locally under
`target/xn-cross-platform/`.

The runtime experiment prepares the consented Voice-Zero Caro Davy reference as
a 434,296-byte voice state before starting synthesis. It then omits the Mimi
encoder from the runtime GGUF, keeps one conditioned base state warm, pipelines
autoregressive generation into a bounded two-frame decoder queue, and checks
cancellation between frames. This is a viable provider design: WAV import can
perform the expensive voice preparation once, while ordinary synthesis loads
only the voice state. It still requires a recovery path if a model update makes
an existing state incompatible.

The initial Apple M4 comparison used the same natural reference, 60-code-point
text, seed 42, two warmups, ten measured iterations, and each runtime's best
tested CPU thread count. RTF is the meaningful completion comparison because
the engines produced different durations: 3.76 seconds for XN and 3.20 seconds
for sherpa. XN's GGUF loader is memory-mapped, so its load timing does not imply
that every model page was faulted during initialization; peak RSS includes the
subsequent warmups and measurements.

| Runtime | Threads | Load | Voice-state prompt | First audio p50 / p95 | Completion p50 / p95 | RTF p50 / p95 | Native peak RSS | Runtime model |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| XN Q8_0 | 4 | 24 ms | 47 ms | 21.6 / 22.3 ms | 390 / 400 ms | 0.1037 / 0.1063 | 246.42 MB | 129.72 MB |
| XN Q8_0 | 2 | 64 ms | 68 ms | 24.8 / 25.1 ms | 424 / 430 ms | 0.1127 / 0.1143 | 242.93 MB | 129.72 MB |
| XN unquantized f32 compute | 4 | 42 ms | 77 ms | 42.6 / 47.1 ms | 627 / 649 ms | 0.1667 / 0.1725 | 667.45 MB | 235.74 MB |
| sherpa-onnx INT8 | 2 | 297 ms | Included in synthesis | 341 / 351 ms | 445 / 458 ms | 0.1392 / 0.1430 | 653.74 MB | 198.55 MB |

Four XN workers were the best M4 operating point tested. One worker produced
0.1498 median RTF, two produced 0.1127, four produced 0.1037, and six regressed
to 0.1161. Sherpa also regressed with four workers. XN Q8 output was
byte-identical across fresh one-, two-, four-, and six-worker processes and
across all ten measured iterations. Two after-first-audio stops at each tested
thread count retained exactly one 80 ms PCM frame; with four workers, the
direct engine loop stopped in 27.6--27.9 ms from synthesis start. These are
engine-loop results, not yet the race and acknowledgement guarantees of an
UtterPipe adapter.

The same XN harness and assets were then built from isolated checkouts on both
x86 development hosts. The table compares XN profiles only: every row uses the
same natural prepared voice state, text, seed, two warmups, and ten measured
runs. Thread counts are the best operating points observed in the bounded
sweep, rather than a universal default.

| Host / profile | Threads | First audio p50 / p95 | Completion p50 / p95 | RTF p50 / p95 | Native peak RSS | Runtime model |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Apple M4 macOS, Q8_0 | 4 | 21.6 / 22.3 ms | 390 / 400 ms | 0.1037 / 0.1063 | 246.42 MB | 129.72 MB |
| Apple M4 macOS, f32 | 4 | 42.6 / 47.1 ms | 627 / 649 ms | 0.1667 / 0.1725 | 667.45 MB | 235.74 MB |
| Windows 11, Ryzen 7 9800X3D, Q8_0 | 8 | 112 / 121 ms | 564 / 585 ms | 0.1500 / 0.1556 | 233.07 MB | 129.72 MB |
| Windows 11, Ryzen 7 9800X3D, f32 | 4 | 28.5 / 34.9 ms | 373 / 381 ms | 0.0992 / 0.1013 | 652.42 MB | 235.74 MB |
| Manjaro Linux, Core i9-12900K, Q8_0 | 8 | 135 / 145 ms | 706 / 720 ms | 0.1877 / 0.1915 | 238.68 MB | 129.72 MB |
| Manjaro Linux, Core i9-12900K, f32 | 4 | 38.9 / 42.3 ms | 701 / 736 ms | 0.1863 / 0.1958 | 657.63 MB | 235.74 MB |

Q8 is the clear speed-and-memory choice on Apple Silicon. On both x86 hosts,
unquantized f32 reduces first-audio latency substantially and is at least as
fast in completion throughput, while Q8 saves approximately 419 MB of peak RSS.
The provider should therefore expose a quality/performance profile and choose
platform-aware defaults only after the protocol-level benchmark is complete.
The best tested worker count is four for f32 on all three hosts, four for Q8 on
the M4, and eight for Q8 on both x86 hosts. A production default should derive
from topology or a conservative platform rule; it must not assume that more
logical CPUs always help.

The six-case PocketTTS.cpp regression corpus was regenerated through both XN
Q8 and XN unquantized execution on macOS. The provisional FP32-relative overlay
detector passed every case, including `runtime`, numbers and punctuation, and
the longer message. Q8/FP32 high-frequency ratios ranged from 1.01 through
2.21; the separate benchmark sentence measured 4.38. Platform-local benchmark
comparisons also passed at 1.80 on Windows and 1.85 on Linux, all below the
known failure threshold of 20. This is promising severe-artifact regression
evidence, not an intelligibility or perceptual-quality certification.

The official XN voice-preparation utility also encoded three additional pinned
CC0 Voice-Zero references: Bill Boerst, Peter Yearsley, and Stuart Bell. Q8 and
f32 both synthesized all three. Peter and Stuart had FP32-relative
high-frequency ratios of 1.12 and 0.99. Bill at seed 42 was a review outlier:
its ratio was 91.98, but the absolute high-frequency fraction was 0.0145 and
remained below the detector's 0.02 failure floor. Repeating Bill at seeds 7 and
2026 produced ratios of 1.44 and 0.60. The outlier therefore does not follow the
Q8 profile consistently, but its saved Q8/FP32 pair remains a mandatory human
listening sample. This demonstrates why the detector needs both relative and
absolute calibration and cannot replace listening.

The first provider-neutral `utterpipe-acoustic-gate` smoke re-evaluated those
three Q8/f32 pairs as bounded PCM16 WAV inputs. The calibrated severe-overlay
screen passed all three; XN Q4_K correctly failed the same detector as a known
bad positive control with a 31.9-times f32 high-frequency ratio and 9.9%
absolute high-frequency energy. The natural-voice gate nevertheless failed on
Peter Yearsley because Q8 contained 77 full-scale PCM16 samples out of 80,640
and f32 contained 80. This is not a Q8-only regression, but it means neither
unscaled profile satisfies a zero-clipping release policy for that voice.
Pre-conversion instrumentation confirmed real decoder overshoot rather than
PCM endpoint rounding: at the same seed the exact April Q8 decoder reached
`1.0863183` with eight samples outside `[-1, 1]`, while f32 reached `1.0824773`
with six samples outside the range. This shared behavior is not a Q8
regression.

The first experimental bundle declared a fixed `0.9` output gain. Applying it
to each decoded float before PCM16 conversion is safe for the bounded streaming
path and yields byte-identical complete and incremental audio; it does not
inspect or normalize a whole utterance. The initial Q8 Peter replay then had a
PCM maximum of 32,036 and zero full-scale samples, and the optimized real-model
integration test passed install, voice preparation, bounded streaming, output
rejection, cancellation, warm reuse, and lease cleanup with that field.

The full signal corpus proved that margin insufficient. Its 84 cases combine
four pinned natural voices, seeds 7, 42, and 2026, upstream's long quantization
paragraph and five stress sentences, and Agent Speak status/cancellation text.
Every Q8 case was generated twice in a separately spawned provider process and
compared with an f32 XN control at the same fixed gain. All replay pairs were
deterministic and no severe overlay signature fired, but twelve Peter Yearsley
cases exceeded the zero-clipping rule; eleven affected Q8 and ten affected f32.
The retained f32 diagnostics reached a raw peak of `1.3877968`.

A complete rerun at `0.7` removed all clipping and passed the original signal
gate, but its highest Q8 PCM peak was `0.9981384`, too close to full scale to be
a useful reserve. The release policy now independently requires peak fraction
at most `0.95` as well as zero clipping. The final `0.65` rerun passed that
stricter gate across all 84 cases: maximum Q8 and f32 peaks were `0.9268494` and
`0.9020386`, clipped-sample and severe-overlay counts were zero, all 84 Q8
replay pairs were deterministic, and the largest Q8/f32 high-frequency ratio
was `2.65`, far below the calibrated `20` failure threshold. This retains
streaming behavior without adding a whole-utterance limiter. The optimized
real-model integration test also passed the final bundle through install,
voice preparation, bounded synthesis, cancellation, warm reuse, and cleanup.

The original three-case smoke is superseded for signal coverage by the 84-case
matrix. The reusable tool represents absent required evidence as `incomplete`,
retains failures/outliers plus a stable listening sample, and never copies
transcript text or local paths into its report. The final run retained eight
spectral outliers and one stable pseudorandom case.

Pinned `whisper.cpp` v1.9.2 with the full `small.en` model then transcribed the
first Q8 replay and f32 baseline for all 84 cases. Q8 mean WER/CER were
`0.1777`/`0.1173`, versus `0.2483`/`0.1793` for f32; Q8 was worse in only two
cases. The maximum positive Q8-minus-f32 WER and CER deltas were `0.1429` and
`0.0616`. Release limits of `0.15` and `0.07` therefore pass this corpus. As a
calibration check, the same recognizer and limits failed all six known-bad
PocketTTS.cpp stock-INT8 clips relative to its clean fp32 output, including the
missing “runtime,” word substitutions, and truncated clauses. Absolute WER was
not used as a hard per-case limit because Whisper rendered number words and the
one-word “Yes.” inconsistently in both otherwise acceptable profiles.

Two offline TTS-naturalness metrics were then tested against the same positive
control. NISQA-TTS v1 failed calibration: it scored the corrupted INT8 longer
message `4.217` versus `4.159` for clean fp32, numbers `3.974` versus `3.668`,
and short status `3.888` versus `3.705`. It is therefore not accepted as release
evidence. UTMOS22 Strong separated five of the six corrupted clips, scoring
them `2.291`--`2.630` versus clean baselines of `3.436`--`4.401`; the short
status escaped UTMOS but was already rejected by ASR.

Pinned UTMOS over the full XN matrix gave mean Q8/f32 scores of
`4.1875`/`3.8822`, a mean Q8 improvement of `0.3053`, and a worst per-case
improvement of `-0.5002`. The release plan requires every Q8 case to stay above
`-0.75`. The scorer wrapper verifies a clean SpeechMOS revision and exact model
SHA-256, and UtterPipe consumes only precomputed scores and provenance; neither
the runtime nor gate downloads or executes the metric. The combined signal,
ASR, and perceptual gate passes all 84 cases. On 2026-08-12 the project
maintainer also accepted all five retained candidate/baseline pairs. Three Q8
candidates were clean; two had tiny candidate-only defects but remained fully
intelligible and acceptable. The hash-bound review record is checked in beside
the XN experiment. The April Q8 profile therefore clears the acoustic release
gate for catalog promotion.

Promotion validation at provider commit
`134a0c5b95ff238f3da786815938e86dba2fcb2b` then closed the first-run path.
The authenticated macOS command downloaded both gated files from the pinned
official revision, reproduced the exact 148,242,752-byte Q8 hash, installed it,
and kept its source cache files private. Fresh Windows and Linux checkouts each
verified the transferred official source hashes and independently reproduced
that same Q8 artifact. All three platforms passed the optimized real-model
integration covering install, voice preparation, incremental synthesis, output
bounds, cancellation, warm reuse, management flow, and lease cleanup. The
Windows pass also proved that the hashed embedded configuration retains its LF
byte identity under a normal checkout.

Listener acceptance on 2026-08-10 passed every saved XN sample, including the
matched Q8/f32 outputs from all three operating systems and all four natural
voices. No noticeable quality difference was heard between XN Q8 and f32. Both
Bill Boerst profiles add the same leading "ah" before the first word; because
the behavior is present in the f32 control as well as Q8, it is recorded as a
voice/model behavior rather than an XN quantization regression.

At a fixed seed, each host produced byte-identical PCM across its own repeated
runs and thread sweep. PCM hashes differed across hosts, so XN must not promise
cross-platform acoustic reproducibility. Every after-first-frame engine stop
completed promptly. The promoted provider adapter subsequently passed UtterPipe
acknowledgement ordering and zero post-acknowledgement audio on every host.

Additional M4 quantization probes showed why quantized profiles need acoustic
and performance gates rather than a size-only ranking:

| XN profile | Runtime model | First audio p50 / p95 | RTF p50 / p95 | Native peak RSS | Artifact probe |
| --- | ---: | ---: | ---: | ---: | --- |
| Q8_0 | 129.72 MB | 21.6 / 22.3 ms | 0.1037 / 0.1063 | 246.42 MB | Passed |
| Q6_K | 111.44 MB | 60.7 / 72.6 ms | 0.2008 / 0.2255 | 228.52 MB | Passed; slower than Q8_0 |
| Q4_K | 91.97 MB | 52.8 / 59.1 ms | 0.1922 / 0.1966 | 205.41 MB | Failed with severe high-frequency overlay artifacts |

Q4_K is rejected for this model generation. Q6_K is not a sensible default
because its modest memory saving costs roughly twice Q8's completion time, but
it can remain an experimental low-memory profile if broader listening passes.
XN now clears the three-platform engine-probe gate for a bounded UtterPipe
adapter. The broader corpus, human listening, long-text handling,
ASR/perceptual metrics, protocol conformance, and bootstrap evidence described
above complete its promotion gates.

The promoted provider pins project-fork commit
`4dbd8d6832cf4e093d08a1bd4666a08783345e7b`. It loads the April Q8 GGUF and
official SentencePiece tokenizer, prepares one compact XN voice state, keeps
conditioned TTS and Mimi decoder states warm, sentence-splits long input, and
connects generation to decoding with bounded backpressure. The public protocol
and ignored real-model tests cover bundle preparation, leases, exact
byte/digest accounting, output rejection, cancellation acknowledgement
ordering, zero post-acknowledgement audio, warm reuse, and cross-platform
provider behavior.

For bootstrap, retaining the April Mimi voice encoder in the Q8 GGUF increases
the model from 128,741,024 bytes to 148,242,752 bytes. The full evaluation GGUF
has SHA-256
`a9548b363f990faca0614dc0533d80b11be80ad0b6ac781b6f42a58dd1659ece`.
It recreated the Caro Davy state byte-for-byte from the original 44.1 kHz WAV
(434,296 bytes, SHA-256
`596be2ac4d1faef704ded1f98c3f639c8a5e2027f91e063fefd0d82d2f35cec9`),
and the adapter synthesized successfully from that recreated state. The
provider-side preparation path mirrors upstream PCM scaling, loudness
normalization, resampling, ten-second trimming, and model-generation metadata.
This supports one self-contained quick-start bundle for both voice import and
ordinary inference instead of requiring users to retain separate f32 encoder
weights. These hashes describe local evaluation artifacts, not a published
catalog entry.

The first full UtterPipe run used that bundle through the provider's versioned
store, a freshly prepared Caro Davy state, four XN threads, two warmups, ten
measurements, and two after-first-audio cancellations on the Apple M4 host.
Runtime initialization took 362 ms. Median/p95 first audio was 23.6/34.3 ms,
completion was 483/500 ms, and real-time factor was 0.1059/0.1097 for a stable
4.56-second, 57-frame output. Sampled direct-process RSS was 278.90 MB steady
and 279.25 MB peak. Both cancellations were accepted; median acknowledgement
was 0.109 ms, terminal cleanup was 6.50 ms, and no audio appeared after either
acknowledgement. A subsequent health request succeeded in the same process.
The experimental executable containing both sherpa and XN engines was 27.63 MB,
and the installed model/reference/state store was 149.67 MB with zero synthesis
growth. This cleared the macOS framing, backpressure,
acknowledgement-ordering, post-ack silence, warm-reuse, and clean-shutdown
gates; the provider-level table above records the matching Windows and Linux
completion.

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

They are inputs for the official implementation and selected XN preparation
path, not ready-to-install provider bundles. The quick start therefore performs
explicit Hugging Face authentication and acceptance, downloads both matching
files, converts and verifies the runtime model, and constructs the complete
self-describing bundle.

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
