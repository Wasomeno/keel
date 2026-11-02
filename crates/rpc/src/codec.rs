//! 4-byte big-endian length, then one JSON value. The length stays on the duplex.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::MAX_FRAME_BYTES;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame is too large")]
    TooLarge,
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub fn encode_frame(json: &[u8]) -> Result<Vec<u8>, FrameError> {
    if json.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    let len = u32::try_from(json.len()).map_err(|_| FrameError::TooLarge)?;
    let mut out = Vec::with_capacity(4 + json.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(json);
    Ok(out)
}

pub async fn write_frame<W>(writer: &mut W, json: &[u8]) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let frame = encode_frame(json)?;
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_frame<R>(reader: &mut R) -> Result<Vec<u8>, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut head = [0u8; 4];
    reader.read_exact(&mut head).await?;
    let len = usize::try_from(u32::from_be_bytes(head)).unwrap_or(usize::MAX);
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    Ok(body)
}

impl From<FrameError> for io::Error {
    fn from(err: FrameError) -> Self {
        match err {
            FrameError::Io(err) => err,
            FrameError::TooLarge => {
                io::Error::new(io::ErrorKind::InvalidData, "frame is too large")
            }
        }
    }
}

/// Cancel-safe frame reader. Partial bytes live in `buf`, not in a future, so a
/// `select!` or `timeout` that drops `next_raw` mid-frame loses nothing.
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    /// Header plus body, as the frame sat on the wire.
    pub async fn next_raw<R>(&mut self, reader: &mut R) -> Result<Vec<u8>, FrameError>
    where
        R: AsyncRead + Unpin,
    {
        loop {
            let mut need = 4;
            if self.buf.len() >= 4 {
                let head = [self.buf[0], self.buf[1], self.buf[2], self.buf[3]];
                let len = usize::try_from(u32::from_be_bytes(head)).unwrap_or(usize::MAX);
                if len > MAX_FRAME_BYTES {
                    return Err(FrameError::TooLarge);
                }
                need = 4 + len;
                if self.buf.len() >= need {
                    return Ok(self.buf.drain(..need).collect());
                }
            }
            self.buf.reserve((need - self.buf.len()).max(8 * 1024));
            // `read_buf` is cancel-safe: on cancel no bytes were consumed.
            if reader.read_buf(&mut self.buf).await? == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
            }
        }
    }

    /// Body only.
    pub async fn next<R>(&mut self, reader: &mut R) -> Result<Vec<u8>, FrameError>
    where
        R: AsyncRead + Unpin,
    {
        let mut raw = self.next_raw(reader).await?;
        raw.drain(..4);
        Ok(raw)
    }
}
