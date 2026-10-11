//! Bounds on what an MCP server can make the client buffer. A server is
//! operator-configured, but a remote one is reached over the network and a
//! local one can misbehave: a line or event that never ends, or an endless
//! response body, would otherwise grow until the daemon runs out of memory.

use futures_util::{Stream, StreamExt};
use tokio::io::AsyncBufRead;

/// The largest single message accepted from a server.
pub(crate) const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// One `\n`-terminated line (newline included, as `read_line` gives it),
/// refused once it passes `limit` bytes. `Ok("")` at end of input.
pub(crate) async fn read_line_capped<R>(reader: &mut R, limit: usize) -> Result<String, String>
where
    R: AsyncBufRead + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut line = String::new();
    let n = reader.take(limit as u64 + 1).read_line(&mut line).await.map_err(|e| e.to_string())?;
    if n > limit {
        return Err(format!("message over {limit} bytes"));
    }
    Ok(line)
}

/// A whole response body as text, refused once it passes `limit` bytes.
pub(crate) async fn collect_capped<S, E>(mut body: S, limit: usize) -> Result<String, String>
where
    S: Stream<Item = Result<bytes::Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut buf = Vec::new();
    while let Some(chunk) = body.next().await {
        buf.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
        if buf.len() > limit {
            return Err(format!("response over {limit} bytes"));
        }
    }
    String::from_utf8(buf).map_err(|e| format!("response isn't UTF-8: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_line_over_the_limit_is_refused_and_one_within_it_is_read() {
        let mut reader = tokio::io::BufReader::new(&b"{\"ok\":1}\n0123456789012345678901234567890123456789\n"[..]);
        assert_eq!(read_line_capped(&mut reader, 32).await.unwrap(), "{\"ok\":1}\n");
        let err = read_line_capped(&mut reader, 32).await.unwrap_err();
        assert!(err.contains("32"), "{err}");
        let mut empty = tokio::io::BufReader::new(&b""[..]);
        assert_eq!(read_line_capped(&mut empty, 32).await.unwrap(), "");
    }

    #[tokio::test]
    async fn a_body_over_the_limit_is_refused_and_one_within_it_is_text() {
        let chunks = |parts: &[&'static str]| {
            futures_util::stream::iter(
                parts
                    .iter()
                    .map(|p| Ok::<_, std::convert::Infallible>(bytes::Bytes::from_static(p.as_bytes())))
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(collect_capped(chunks(&["{\"a\":", "1}"]), 32).await.unwrap(), "{\"a\":1}");
        let err = collect_capped(chunks(&["0123456789", "0123456789", "0123456789", "0123"]), 32)
            .await
            .unwrap_err();
        assert!(err.contains("32"), "{err}");
    }
}
