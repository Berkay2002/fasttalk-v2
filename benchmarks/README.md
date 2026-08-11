# Performance experiments

This directory keeps the measured engineering record for FastTalk. It is
separate from the release gate because failed candidates and intermediate
spikes are useful evidence too.

The final staged comparison is
[2026-08-11-rtx3090-matrix](2026-08-11-rtx3090-matrix/README.md). Its control is
[2026-08-11-rtx3090-post-audit](2026-08-11-rtx3090-post-audit/README.md).
The follow-up
[Qwen3.5 refusal-behavior comparison](2026-08-11-qwen35-decensor/README.md)
tests three less-censored variants against the selected official post-trained
baseline without changing the default profile.
The original [2026-08-11-rtx3090](2026-08-11-rtx3090/README.md) archive is
retained as superseded experiment history.
Its `summary.csv` is intended for plotting and comparison. Its `evidence`
directory contains the small raw JSON outputs and a SHA-256 manifest copied
from the ignored `artifacts` workspace.

## Recording rules

- Record the hardware, software, model revision or quantization, context,
  concurrency, and workload.
- Keep observed values separate from interpretations and product decisions.
- Preserve failed runs. A failed gate is still a valid result.
- Compare models only when the workload and harness are comparable.
- Label derived latency bounds differently from event timestamps.
- Report both total GPU usage and worker-attributable usage when available.
- Keep large models, native binaries, WAV output, and verbose logs out of git.

Run `scripts/Snapshot-BenchmarkEvidence.ps1` for the control archive or
`scripts/Snapshot-MatrixEvidence.ps1` for the cross-profile archive to refresh
the portable evidence copies and hashes.
