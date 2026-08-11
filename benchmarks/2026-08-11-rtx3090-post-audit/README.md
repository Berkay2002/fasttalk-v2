# RTX 3090 post-audit baseline

This archive is the corrected control for the final cross-profile matrix. It
supersedes the integrated latency, clause streaming, barge-in, cancellation,
and soak claims in the original `2026-08-11-rtx3090` archive. The original
files remain available as experiment history.

## Result

The Qwen3.5 9B Q5_K_M, Nemotron 3.5 ASR Q8, and Magpie v2602 F16 profile
passes all 14 formal release checks. The final 20-turn run synthesized every
LLM clause, used post-AEC prerecorded capture, and measured audible
interruption through the WASAPI output callback. The corrected soak completed
131 full turns in 30.111 minutes without a failed turn, worker transition, or
OOM.

The interruption path uses a 20 ms post-AEC energy onset so playback does not
wait for model inference. Silero remains responsible for speech state, and
Smart Turn v3.2 remains responsible for semantic endpoint completion. This
separation keeps the endpoint model-backed while bounding audible barge-in.

Magpie delivers incremental PCM. On interruption, client delivery and
playback stop immediately, while the already-started bounded native clause is
drained in the background. This avoids a reproducible native worker crash on
an abrupt HTTP body disconnect. The evidence does not claim that native
synthesis compute stops at the cancellation timestamp.

## Corrected integrated measurements

| Metric | Observed | Gate | Result |
| --- | ---: | ---: | --- |
| End of speech to first audio p50 | 604.667 ms | at most 1,200 ms | Pass |
| End of speech to first audio p95 | 1,186.430 ms | at most 1,800 ms | Pass |
| Warm LLM first token p95 | 99.314 ms | at most 900 ms | Pass |
| LLM generation minimum | 47.364 tok/s | at least 20 tok/s | Pass |
| ASR partial event maximum | 23.958 ms | at most 250 ms | Pass |
| Speech onset to interrupt p95 | 42.038 ms | observation | Pass |
| Interrupt to output callback p95 | 9.778 ms | observation | Pass |
| Speech onset to silence maximum | 51.734 ms | at most 150 ms | Pass |
| Warm GPU total maximum | 12,061 MiB | at most 23,040 MiB | Pass |
| Synthesized clauses per turn | 2 minimum | at least 2 | Pass |
| Active LLM delivery cancellation | 0.105 ms | below 2,000 ms | Pass |
| Active TTS delivery cancellation | 0.030 ms | below 2,000 ms | Pass |
| Conversation soak | 30.111 min, 131 turns | at least 30 min, no failures | Pass |

The 20-turn fixture is a deterministic integration workload, not a broad
language-quality benchmark. The six prerecorded acoustic scenarios exercise
quiet speech, hesitation, short and long utterances, deterministic noise, and
speaker playback with a paired render reference. They do not replace corpus
WER or blind listening tests.

## Provenance

The raw JSON copies and their SHA-256 hashes are in
[`evidence`](evidence/manifest.json). Machine-readable headline metrics are in
[`summary.csv`](summary.csv). The manifest records source commit `3adcaa9`.

The copied installer evidence predates the post-audit runtime changes. It is
retained for provenance but is not the final packaging result. The installer
must be rebuilt and retested after the benchmark matrix selects the default
profile. Authenticode and a true clean offline VM remain external checks.
