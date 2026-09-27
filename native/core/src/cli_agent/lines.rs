//! Bounded vendor framing. Partial bytes belong to the reader, so cancelling
//! a pending read (polling, shutdown, sign-in detection) cannot discard them.
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

pub(super) const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;

/// Keep framing overflow distinguishable from unsupported handshakes. It must
/// not trigger another execution route after rejecting the first runtime.
#[derive(Debug)]
pub(super) struct ProtocolLineLimit(pub usize);
impl std::fmt::Display for ProtocolLineLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Vendor protocol line exceeded the {} byte limit; runtime stopped",
            self.0
        )
    }
}
impl std::error::Error for ProtocolLineLimit {}

#[derive(Debug, PartialEq)]
pub(super) enum Line {
    Text(String),
    TooLong,
}

pub(super) struct BoundedLines<R> {
    reader: BufReader<R>,
    pending: Vec<u8>,
    limit: usize,
    discarding: bool,
}

impl<R: AsyncRead + Unpin> BoundedLines<R> {
    pub(super) fn new(reader: R, limit: usize) -> Self {
        Self {
            reader: BufReader::new(reader),
            pending: Vec::new(),
            limit,
            discarding: false,
        }
    }

    /// Report overflow immediately, without waiting for a newline. The next
    /// call discards the rest of that line with bounded storage and resumes
    /// at the following line. Never return fragments of oversized diagnostics.
    pub(super) async fn next_line(&mut self) -> std::io::Result<Option<Line>> {
        loop {
            // fill_buf is cancellation safe; consume and state updates below
            // have no suspension point between them.
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                if self.pending.len() > self.limit {
                    self.pending.clear();
                    return Ok(Some(Line::TooLong));
                }
                let text = String::from_utf8_lossy(&self.pending).into_owned();
                self.pending.clear();
                return Ok(Some(Line::Text(text)));
            }
            let newline = available.iter().position(|&b| b == b'\n');
            let content_len = newline.unwrap_or(available.len());
            let consumed = content_len + usize::from(newline.is_some());
            if self.discarding {
                self.reader.consume(consumed);
                self.discarding = newline.is_none();
                continue;
            }
            let content = &available[..content_len];
            let total = self.pending.len().saturating_add(content.len());
            // Allow one extra CR while waiting to distinguish CRLF from a
            // literal trailing CR. Neither delimiter counts against the cap.
            let trailing_cr = content.last().or_else(|| self.pending.last()) == Some(&b'\r');
            if total > self.limit && !(total == self.limit + 1 && trailing_cr) {
                self.pending.clear();
                self.discarding = newline.is_none();
                self.reader.consume(consumed);
                return Ok(Some(Line::TooLong));
            }
            self.pending.extend_from_slice(content);
            self.reader.consume(consumed);
            if newline.is_some() {
                if self.pending.last() == Some(&b'\r') {
                    self.pending.pop();
                }
                let text = String::from_utf8_lossy(&self.pending).into_owned();
                self.pending.clear();
                return Ok(Some(Line::Text(text)));
            }
        }
    }

    /// Protocol truncation would change the message. Fail the caller instead.
    pub(super) async fn next_protocol_line(&mut self) -> anyhow::Result<Option<String>> {
        match self.next_line().await? {
            Some(Line::Text(line)) => Ok(Some(line)),
            Some(Line::TooLong) => Err(ProtocolLineLimit(self.limit).into()),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn cancelled_reads_preserve_partial_utf8() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let mut lines = BoundedLines::new(reader, 32);
        writer.write_all(b"prefix \xc3").await.unwrap();
        for _ in 0..3 {
            tokio::select! {
                biased;
                line = lines.next_line() => panic!("unexpected complete line: {line:?}"),
                _ = tokio::task::yield_now() => {}
            }
        }
        writer.write_all(b"\xa9\r\n").await.unwrap();
        assert_eq!(
            lines.next_line().await.unwrap(),
            Some(Line::Text("prefix é".into()))
        );
    }

    #[tokio::test]
    async fn overflow_is_immediate_and_resynchronizes_without_returning_fragments() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let mut lines = BoundedLines::new(reader, 8);
        writer.write_all(b"0123456789").await.unwrap();
        assert_eq!(lines.next_line().await.unwrap(), Some(Line::TooLong));
        assert!(lines.pending.is_empty());
        writer.write_all(b"secret suffix\nsafe\n").await.unwrap();
        assert_eq!(
            lines.next_line().await.unwrap(),
            Some(Line::Text("safe".into()))
        );
    }

    #[tokio::test]
    async fn length_boundary_crlf_empty_line_and_eof() {
        for (input, expected) in [
            (b"12345678\n".as_slice(), Line::Text("12345678".into())),
            (b"12345678\r\n", Line::Text("12345678".into())),
            (b"12345678", Line::Text("12345678".into())),
            (b"12345678\r", Line::TooLong),
            (b"123456789\n", Line::TooLong),
            (b"\n", Line::Text(String::new())),
        ] {
            let mut lines = BoundedLines::new(input, 8);
            assert_eq!(lines.next_line().await.unwrap(), Some(expected));
            assert_eq!(lines.next_line().await.unwrap(), None);
        }
    }
}
