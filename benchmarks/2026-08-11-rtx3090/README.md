# RTX 3090 feasibility and release measurements

## Result

Qwen3.6 27B Q4_K_M was functionally compatible, but its representative
LLM-only run missed the fixed 20 token/s minimum. Qwen3.5 9B Q5_K_M passed
that gate with substantial memory headroom and became the compatibility
profile. The resulting 20-turn prerecorded conversation run passed every
latency, throughput, and VRAM gate. A 30.208-minute stability run completed
124 turns without an OOM or failed worker poll.

The evidence supports a product decision, not a general claim that the 9B
model is better. Qwen3.6 is the larger model and may produce better answers,
but it did not meet this product's speed constraint on this configuration.

## Test environment

| Item | Value |
| --- | --- |
| Date | 2026-08-10 to 2026-08-11 |
| OS | Windows 11 build 26200 |
| GPU | NVIDIA GeForce RTX 3090, 24,576 MiB |
| Driver | NVIDIA 610.47 |
| CUDA toolkit | 13.3 |
| Rust | 1.97.1, x86_64-pc-windows-msvc |
| llama.cpp server | Native CUDA build, 16,384 token context, 4 parallel slots |
| ASR | Nemotron 3.5 ASR Streaming 0.6B Q8, CUDA |
| TTS | Magpie v2602 F16 with NanoCodec F16, CUDA |
| Main speech fixture | `nemo-speech.cpp/test_files/asr/wav/test/jfk.wav` |
| Background conditions | Normal desktop applications remained open |

GPU memory values are process-wide device totals unless the row explicitly
says worker delta. Desktop applications account for the difference between
baseline and worker delta.

## LLM-only comparison

| Profile | Warm TTFT p50 | Warm TTFT p95 | Generation min | Generation median | Warm GPU total | Decision |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Qwen3.6 27B Q4_K_M | 173.545 ms | 177.395 ms | 17.197 tok/s | 17.489 tok/s | 20,545 MiB | Reject for default profile |
| Qwen3.5 9B Q5_K_M | 65.097 ms | 68.446 ms | 47.364 tok/s | 48.700 tok/s | 9,815 MiB | Select compatibility profile |

Both runs used 20 warm first-token samples and 5 generation samples. The raw
Qwen3.6 samples are in
[`qwen36-llm.json`](evidence/qwen36-llm.json), and the Qwen3.5 samples are in
[`qwen35-llm.json`](evidence/qwen35-llm.json).

Parallel slot counts from 1 through 4 and Q8 KV cache were also tried during
the Qwen3.6 spike. None reached 20 tok/s under the representative desktop
load. Their exact per-run samples were not retained, so this is experiment
history rather than quantitative evidence.

An earlier Qwen3.6 pipeline-composition artifact reported a 35.623 tok/s
minimum on a different short-generation workload. It is preserved, but was
not used for the model choice because the later dedicated LLM benchmark is the
comparable admission test. This distinction prevents an attractive number
from overriding a stricter measurement.

## STT-only

Nemotron produced zero transcript mismatches over 20 repetitions of the JFK
fixture and processed audio at 35.0679 times real time. The initial harness
estimated a 164.5626 ms partial-update upper bound by adding a configured 160
ms chunk to 4.5626 ms average compute time. That is a derived scheduling
bound, not a received-event timestamp.

The integrated harness later measured actual partial events. Its maximum was
24.003 ms across 540 updates. The two numbers answer different questions and
should not be compared as if they came from the same clock.

## TTS-only and transport spikes

| Backend and transport | Samples | First PCM p50 | First PCM p95 | RTF max | Cancel max | Finding |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Magpie request-buffered prototype | 20 | 953.489 ms | 1,078.276 ms | 0.2041 | 0.823 ms | Synthesis was fast, delivery was not truly streaming |
| Magpie chunked PCM transport | 3 | 43.197 ms | 43.810 ms | 0.2076 | 2.613 ms | True incremental PCM delivery confirmed |
| Kokoro CPU phrase transport, v1 bundle | 1 | 3,734.571 ms | n/a | n/a | n/a | Valid fallback, too slow for preferred path |
| Kokoro CPU phrase transport, compact bundle | 1 | 5,106.072 ms | n/a | n/a | n/a | Valid fallback, too slow for preferred path |

The chunked Magpie transport returned 158 PCM reads over 2,090.029 ms, with
the first read at 45.809 ms. This is why the implementation claims true audio
streaming rather than treating fast complete synthesis as streaming.

## Combined configurations

| Configuration | Baseline GPU | Warm GPU total | Worker delta | Finding |
| --- | ---: | ---: | ---: | --- |
| Qwen3.6 + ASR + Magpie | 2,866 MiB | 22,450 MiB | 19,584 MiB | Fit below 22.5 GiB gate, but left little operational headroom |
| Qwen3.5 + ASR | 3,169 MiB | 11,071 MiB | 7,902 MiB | Large TTS admission margin |
| Qwen3.5 + ASR + Magpie | 3,143 MiB | 12,000 MiB | 8,857 MiB | Selected preferred runtime combination |

The runtime now stores these measured worker deltas in a named profile. It
queries the installed GPU's total and used memory and subtracts the profile's
reserve instead of hardcoding a 24 GB device limit.

## Integrated 20-turn conversation

The release harness injected the prerecorded JFK audio through the streaming
ASR path, streamed generated tokens into clause-level TTS, captured first PCM,
then drained both streams to completion before starting the next turn.

| Metric | Observed | Gate | Result |
| --- | ---: | ---: | --- |
| End of speech to first audio p50 | 790.322 ms | at most 1,200 ms | Pass |
| End of speech to first audio p95 | 957.825 ms | at most 1,800 ms | Pass |
| Warm LLM first token p95 | 102.336 ms | at most 900 ms | Pass |
| LLM generation minimum | 47.364 tok/s | at least 20 tok/s | Pass |
| ASR partial event maximum | 24.003 ms | at most 250 ms | Pass |
| Barge-in to cancellation maximum | 9.903 ms | at most 150 ms | Pass |
| Warm GPU total maximum | 12,100 MiB | at most 23,040 MiB | Pass |
| TTS real-time factor maximum | 0.2041 | below 1.0 | Pass |
| Conversation soak | 30.208 min, 124 turns | at least 30 min, no OOM | Pass |

The prerecorded scenario suite adds quiet speech, hesitation, a short
acknowledgement, a long question, deterministic background noise, and
speaker-playback fixtures. Those fixtures test the transport and integration
path. They are not a substitute for a broad ASR accuracy corpus or subjective
TTS quality evaluation.

The soak began before the final binary added an explicit `turnFailureCount`
field. Its source JSON directly records 124 completed turns, zero OOMs, zero
worker failures, and process exit code 0. The evidence composer maps the absent
field to zero. This provenance is retained instead of claiming the older
binary emitted a field it did not have.

## Interpretation and next experiments

The largest remaining quality experiments should keep the passing profile as
the control:

1. Compare Qwen3.5 Q8 against Q5_K_M on a fixed prompt set, measuring response
   quality, TTFT, generation rate, and combined VRAM.
2. Compare Nemotron 3.5 ASR Q8 with Nemotron Speech Streaming EN 0.6B on a
   larger noisy English corpus using WER plus partial latency.
3. Run paired listening tests for Magpie against stronger local TTS candidates
   before changing the default. First PCM, RTF, and cancellation remain hard
   constraints.
4. Increase LLM context only with a conversation-recall test. The current UI
   retains 12 messages, so a larger context window alone has no demonstrated
   product benefit.

Machine-readable metrics are in [`summary.csv`](summary.csv). Small raw JSON
outputs and their hashes are in [`evidence`](evidence/manifest.json).

## Packaging verification

The current-user NSIS installer was built from commit `6883f61`. The sanitized
test removed Node, Rust, Python, CUDA toolkit, and related development paths
from the child process environment, silently installed the package, verified
all 26 bundled runtime files by SHA-256, opened the FastTalk window, and then
verified uninstall cleanup.

| Item | Result |
| --- | --- |
| Installer SHA-256 | `36e061185ba9c04fe143c0251847666aeee5662ebd3e1cbf0e07ae9eeff08eef` |
| Current-user install | Pass |
| Sanitized launch | Pass |
| Runtime files hash-verified | 26 |
| Uninstall registry and directory cleanup | Pass |
| Authenticode signing | Not run, no code-signing certificate is installed |
| True clean Windows VM | Not run, Windows Sandbox is absent and Hyper-V access is denied by host policy |
| True offline operation | Not run, the active Ethernet connection cannot be disabled without administrator access |

The sanitized launch is useful packaging evidence, but it is not mislabeled as
a clean-machine or network-disabled result.
