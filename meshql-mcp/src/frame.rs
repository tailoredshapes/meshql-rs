//! Newline framing with bounded retained storage, including rejected fragments.
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
pub(crate) enum Frame {
    Eof,
    Bytes(Vec<u8>),
    Oversized,
}
pub(crate) async fn read<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> std::io::Result<Frame> {
    let mut bytes = Vec::new();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if oversized {
                Frame::Oversized
            } else if bytes.is_empty() {
                Frame::Eof
            } else {
                Frame::Bytes(bytes)
            });
        }
        let end = available.iter().position(|b| *b == b'\n');
        let n = end.unwrap_or(available.len());
        if !oversized {
            if bytes.len() + n > limit {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&available[..n]);
            }
        }
        reader.consume(n + usize::from(end.is_some()));
        if end.is_some() {
            return Ok(if oversized {
                Frame::Oversized
            } else {
                Frame::Bytes(bytes)
            });
        }
    }
}
