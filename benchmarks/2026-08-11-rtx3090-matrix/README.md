# RTX 3090 cross-profile matrix

This archive records the final staged model and runtime-profile benchmark for
FastTalk v2. The corrected post-audit baseline remains the control. Candidate
discovery used the authenticated Hugging Face CLI, and every downloaded
artifact is pinned by repository revision, byte count, and SHA-256 in
[`candidates.json`](candidates.json).

## Selected profile

The selected profile is `rtx3090-qwen35-q5-parakeet-32k`:

- Qwen3.5 9B Q5_K_M through llama.cpp, non-thinking, one true 32K context slot.
- Parakeet CTC 1.1B Q8 through NeMo-Speech.cpp, English streaming ASR.
- Magpie v2602 F16 and NanoCodec F16 through NeMo-Speech.cpp, with incremental PCM.
- Kokoro 82M INT8 on CPU as the automatic memory fallback.

The profile uses measured worker deltas rather than assuming that every GPU has
24 GB. This RTX 3090-specific entry reserves 1,536 MiB, while other hardware
can use another entry in `runtime-profiles.json`.

## Important context correction

llama.cpp divides `--ctx-size` across parallel slots. The earlier `16K,
parallel 4` configuration provided about 4K tokens to one conversation, and a
nominal `32K, parallel 4` configuration provided about 8K. FastTalk currently
has one local user and one active conversation, so the selected profile uses
`32K, parallel 1`. A 12,034-token needle-recall request failed against the
four-slot configuration and passed against the corrected one.

## LLM results

| Candidate | True context | TTFT p95 | Minimum generation | Fixed checks | Warm GPU total | Decision |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Qwen3.5 9B Q5_K_M | 32K | 58.957 ms | 49.230 tok/s | 7/7 | 10,165 MiB | Selected |
| Qwen3.5 9B Q8_0 | 32K | 68.590 ms | 37.885 tok/s | 7/7 | 12,606 MiB | Dominated by Q5 |
| Qwen3 14B Q5_K_M | 32K | 68.878 ms | 29.559 tok/s | 6/7 | 18,160 MiB | Dominated |
| Qwen3.6 27B Q4_K_M | Earlier 16K test | 177.395 ms | 17.197 tok/s | Not rerun on expanded rubric | 20,545 MiB | Below speed gate |

The fixed checks cover exact instruction following, conversation recall,
format constraints, arithmetic, corrected conversation state, a JSON contract,
and long-context recall. This is a deterministic regression rubric, not a
general claim that one model wins broad academic or human-preference evals.

## STT results

| Candidate | Streaming RTF | Derived partial upper bound | Six-fixture WER | Decision |
| --- | ---: | ---: | ---: | --- |
| Nemotron 3.5 ASR Streaming 0.6B Q8 | 35.0679x | 164.5626 ms | 6.78% | Multilingual control |
| Nemotron Speech Streaming EN 0.6B Q8 | 31.9565x | 165.0068 ms | 6.78% | Dominated by control |
| Parakeet CTC 1.1B Q8 | 12.0300x | 173.3001 ms | 1.69% | Selected |

The WER corpus is six prerecorded synthetic fixtures covering quiet speech,
hesitation, a short acknowledgement, a long question, deterministic background
noise, and speaker playback with an AEC reference. It has 59 reference words.
It is useful for a controlled cross-profile comparison but is too small to
replace a public multi-speaker ASR benchmark.

## TTS results

Magpie retained its preferred status. Its three-sample streaming run measured
43.810 ms first PCM p95 and 0.1900 median RTF. A separate transport trace
recorded 158 PCM reads, which proves that the client receives audio before the
native clause finishes. Kokoro remains a valid CPU fallback, but its measured
first PCM was 5,106.072 ms and its delivery is phrase-buffered.

VibeVoice Realtime 0.5B and Qwen3-TTS 0.6B/1.7B are credible quality candidates,
but their official paths require separate Python/PyTorch serving stacks. The
architecture's admission rule rejects unrelated production runtimes, so those
models were pinned at repository revision and deferred before weight download.
The archive includes randomized Magpie/Kokoro A/B WAV pairs in
[`evidence/tts-listening`](evidence/tts-listening). Human preference fields are
left blank; no synthetic MOS or unperformed listening result is claimed.

## End-to-end finalist

| Metric | Observed | Gate | Result |
| --- | ---: | ---: | --- |
| End of speech to first audio p50 | 443.041 ms | at most 1,200 ms | Pass |
| End of speech to first audio p95 | 489.664 ms | at most 1,800 ms | Pass |
| Warm LLM first token p95 | 79.131 ms | at most 900 ms | Pass |
| ASR partial event maximum | 42.073 ms | at most 250 ms | Pass |
| Speech onset to silence p95 | 26.120 ms | at most 150 ms | Pass |
| Warm GPU total maximum | 13,169 MiB | at most 23,040 MiB | Pass |
| Synthesized clauses per turn | 2 minimum | at least 2 | Pass |
| Acoustic scenarios | 6 of 6 | 6 of 6 | Pass |
| Finalist soak | 30.173 min, 134 turns | at least 30 min | Pass |
| Soak failures | 0 turn, 0 worker, 0 OOM | all zero | Pass |

The cancellation evidence measures client delivery after the first live LLM
delta and first live TTS PCM frame. Magpie stops delivery and playback
immediately, then drains the already-started bounded native clause so the
speech worker does not crash. It does not claim cancellation of native compute
inside that clause.

## Packaging result

The selected default was rebuilt as an unsigned current-user NSIS installer.
The locked release-mode workspace suite, frontend production build, and Tauri
bundle completed. A sanitized install test then installed the package, verified
every runtime payload hash, opened the FastTalk window, and removed the app and
uninstall entry cleanly. Authenticode remains unverified because no code-signing
certificate is configured. A true disconnected-host run and an independent
clean Windows VM remain external release checks; the repository does not label
the sanitized same-host test as either one.

## Reproduction and provenance

Machine-readable headline values are in [`summary.csv`](summary.csv). Raw JSON,
WAV listening samples, byte counts, and SHA-256 hashes are in
[`evidence/manifest.json`](evidence/manifest.json). Reusable entry points are:

- `Benchmark-Llama.ps1` for TTFT, throughput, context, and fixed quality checks.
- `Benchmark-ASR.ps1` and `Benchmark-ASR-Corpus.ps1` for streaming performance and WER.
- `Measure-CombinedVram.ps1` for staged memory admission.
- `release-gate --profile ...` for full turns, cancellation, barge-in, and soak.
- `Test-AcousticScenarios.ps1` for the six prerecorded integration scenarios.
- `New-TtsListeningSet.ps1` for blind A/B WAV pairs.
- `Snapshot-MatrixEvidence.ps1` for the immutable evidence copy and hashes.

The Rust toolchain was rechecked during this run: stable 1.97.1 and rustup
1.29.0 were current. The project-local frontend runtime remains Node 24.19.0
with npm 11.17.0.
