use serde::Serialize;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticStream {
    Stdout,
    Stderr,
    Supervisor,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticLine {
    pub stream: DiagnosticStream,
    pub message: String,
}

#[derive(Debug)]
pub struct BoundedDiagnostics {
    capacity: usize,
    maximum_line_bytes: usize,
    lines: VecDeque<DiagnosticLine>,
}

impl BoundedDiagnostics {
    #[must_use]
    pub fn new(capacity: usize, maximum_line_bytes: usize) -> Self {
        Self {
            capacity,
            maximum_line_bytes,
            lines: VecDeque::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, stream: DiagnosticStream, message: impl AsRef<str>) {
        if self.capacity == 0 {
            return;
        }
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
        }
        let redacted = redact(message.as_ref());
        self.lines.push_back(DiagnosticLine {
            stream,
            message: truncate_utf8(&redacted, self.maximum_line_bytes),
        });
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<DiagnosticLine> {
        self.lines.iter().cloned().collect()
    }
}

fn redact(input: &str) -> String {
    const MARKERS: [&str; 5] = [
        "authorization:",
        "hf_token=",
        "api_key=",
        "token=",
        "bearer ",
    ];
    let lower = input.to_ascii_lowercase();
    let Some(index) = MARKERS.iter().filter_map(|marker| lower.find(marker)).min() else {
        return input.to_owned();
    };

    let prefix = &input[..index];
    format!("{prefix}[REDACTED]")
}

fn truncate_utf8(input: &str, maximum_bytes: usize) -> String {
    if input.len() <= maximum_bytes {
        return input.to_owned();
    }
    const ELLIPSIS: &str = "…";
    if maximum_bytes < ELLIPSIS.len() {
        return String::new();
    }
    let mut end = maximum_bytes
        .saturating_sub(ELLIPSIS.len())
        .min(input.len());
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &input[..end], ELLIPSIS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_evict_oldest_lines() {
        let mut diagnostics = BoundedDiagnostics::new(2, 100);
        diagnostics.push(DiagnosticStream::Stdout, "one");
        diagnostics.push(DiagnosticStream::Stdout, "two");
        diagnostics.push(DiagnosticStream::Stdout, "three");
        let messages = diagnostics
            .snapshot()
            .into_iter()
            .map(|line| line.message)
            .collect::<Vec<_>>();
        assert_eq!(messages, ["two", "three"]);
    }

    #[test]
    fn diagnostics_redact_credentials() {
        let mut diagnostics = BoundedDiagnostics::new(2, 100);
        diagnostics.push(
            DiagnosticStream::Stderr,
            "Authorization: Bearer secret-value",
        );
        assert_eq!(diagnostics.snapshot()[0].message, "[REDACTED]");
    }

    #[test]
    fn truncation_keeps_utf8_valid() {
        let truncated = truncate_utf8("abcéfg", 5);
        assert_eq!(truncated, "ab…");
        assert!(truncated.len() <= 5);
    }
}
