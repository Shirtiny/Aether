//! Utilities to work with raw WebSocket frames.

pub mod coding;

#[allow(clippy::module_inception)]
mod frame;
mod mask;
mod utf8;

pub use self::{
    frame::{CloseFrame, Frame, FrameHeader},
    utf8::Utf8Bytes,
};

use crate::{
    error::{CapacityError, Error, ProtocolError, Result},
    protocol::frame::mask::apply_mask,
    Message,
};
use bytes::BytesMut;
use log::*;
use std::io::{self, Cursor, Error as IoError, ErrorKind as IoErrorKind, Read, Write};

/// Read buffer size used for `FrameSocket`.
const READ_BUF_LEN: usize = 128 * 1024;

/// A reader and writer for WebSocket frames.
#[derive(Debug)]
pub struct FrameSocket<Stream> {
    /// The underlying network stream.
    stream: Stream,
    /// Codec for reading/writing frames.
    codec: FrameCodec,
}

impl<Stream> FrameSocket<Stream> {
    /// Create a new frame socket.
    pub fn new(stream: Stream) -> Self {
        FrameSocket { stream, codec: FrameCodec::new(READ_BUF_LEN) }
    }

    /// Create a new frame socket from partially read data.
    pub fn from_partially_read(stream: Stream, part: Vec<u8>) -> Self {
        FrameSocket { stream, codec: FrameCodec::from_partially_read(part, READ_BUF_LEN) }
    }

    /// Extract a stream from the socket.
    pub fn into_inner(self) -> (Stream, BytesMut) {
        (self.stream, self.codec.in_buffer)
    }

    /// Returns a shared reference to the inner stream.
    pub fn get_ref(&self) -> &Stream {
        &self.stream
    }

    /// Returns a mutable reference to the inner stream.
    pub fn get_mut(&mut self) -> &mut Stream {
        &mut self.stream
    }
}

impl<Stream> FrameSocket<Stream>
where
    Stream: Read,
{
    /// Read a frame from stream.
    pub fn read(&mut self, max_size: Option<usize>) -> Result<Option<Frame>> {
        self.codec.read_frame(&mut self.stream, max_size, false, true)
    }
}

impl<Stream> FrameSocket<Stream>
where
    Stream: Write,
{
    /// Writes and immediately flushes a frame.
    /// Equivalent to calling [`write`](Self::write) then [`flush`](Self::flush).
    pub fn send(&mut self, frame: Frame) -> Result<()> {
        self.write(frame)?;
        self.flush()
    }

    /// Write a frame to stream.
    ///
    /// A subsequent call should be made to [`flush`](Self::flush) to flush writes.
    ///
    /// This function guarantees that the frame is queued unless [`Error::WriteBufferFull`]
    /// is returned.
    /// In order to handle WouldBlock or Incomplete, call [`flush`](Self::flush) afterwards.
    pub fn write(&mut self, frame: Frame) -> Result<()> {
        self.codec.buffer_frame(&mut self.stream, frame)
    }

    /// Flush writes.
    pub fn flush(&mut self) -> Result<()> {
        self.codec.write_out_buffer(&mut self.stream)?;
        Ok(self.stream.flush()?)
    }
}

/// A codec for WebSocket frames.
#[derive(Debug)]
pub(super) struct FrameCodec {
    /// Buffer to read data from the stream.
    in_buffer: BytesMut,
    in_buf_max_read: usize,
    /// Buffer to send packets to the network.
    out_buffer: Vec<u8>,
    /// Capacity limit for `out_buffer`.
    max_out_buffer_len: usize,
    /// Buffer target length to reach before writing to the stream
    /// on calls to `buffer_frame`.
    ///
    /// Setting this to non-zero will buffer small writes from hitting
    /// the stream.
    out_buffer_write_len: usize,
    /// Capacity retained after the write buffer has been completely drained.
    max_retained_out_buffer_capacity: usize,
    /// Header and remaining size of the incoming packet being processed.
    header: Option<(FrameHeader, u64)>,
}

impl FrameCodec {
    /// Create a new frame codec.
    pub(super) fn new(in_buf_len: usize) -> Self {
        Self {
            in_buffer: BytesMut::with_capacity(in_buf_len),
            in_buf_max_read: in_buf_len.max(FrameHeader::MAX_SIZE),
            out_buffer: <_>::default(),
            max_out_buffer_len: usize::MAX,
            out_buffer_write_len: 0,
            max_retained_out_buffer_capacity: usize::MAX,
            header: None,
        }
    }

    /// Create a new frame codec from partially read data.
    pub(super) fn from_partially_read(part: Vec<u8>, min_in_buf_len: usize) -> Self {
        let mut in_buffer = BytesMut::from_iter(part);
        in_buffer.reserve(min_in_buf_len.saturating_sub(in_buffer.len()));
        Self {
            in_buffer,
            in_buf_max_read: min_in_buf_len.max(FrameHeader::MAX_SIZE),
            out_buffer: <_>::default(),
            max_out_buffer_len: usize::MAX,
            out_buffer_write_len: 0,
            max_retained_out_buffer_capacity: usize::MAX,
            header: None,
        }
    }

    /// Sets a maximum size for the out buffer.
    pub(super) fn set_max_out_buffer_len(&mut self, max: usize) {
        self.max_out_buffer_len = max;
    }

    /// Sets [`Self::buffer_frame`] buffer target length to reach before
    /// writing to the stream.
    pub(super) fn set_out_buffer_write_len(&mut self, len: usize) {
        self.out_buffer_write_len = len;
    }

    pub(super) fn set_max_retained_out_buffer_capacity(&mut self, capacity: usize) {
        self.max_retained_out_buffer_capacity = capacity;
        self.shrink_empty_out_buffer();
    }

    fn shrink_empty_out_buffer(&mut self) {
        if self.out_buffer.is_empty()
            && self.out_buffer.capacity() > self.max_retained_out_buffer_capacity
        {
            let retained_capacity = self
                .out_buffer_write_len
                .min(self.max_retained_out_buffer_capacity);
            self.out_buffer.shrink_to(retained_capacity);
            // Preserve the configured hard bound even if an allocator cannot honor the shrink.
            if self.out_buffer.capacity() > self.max_retained_out_buffer_capacity {
                self.out_buffer = Vec::new();
            }
        }
    }

    /// Read a frame from the provided stream.
    pub(super) fn read_frame(
        &mut self,
        stream: &mut impl Read,
        max_size: Option<usize>,
        unmask: bool,
        accept_unmasked: bool,
    ) -> Result<Option<Frame>> {
        let max_size = max_size.unwrap_or_else(usize::max_value);

        let mut payload = loop {
            if self.header.is_none() {
                let mut cursor = Cursor::new(&mut self.in_buffer);
                self.header = FrameHeader::parse(&mut cursor)?;
                let advanced = cursor.position();
                bytes::Buf::advance(&mut self.in_buffer, advanced as _);

                if let Some((_, len)) = &self.header {
                    let len = *len as usize;

                    // Enforce frame size limit early
                    if len > max_size {
                        return Err(Error::Capacity(CapacityError::MessageTooLong {
                            size: len,
                            max_size,
                        }));
                    }

                    // Reserve full message length only once, even for multiple
                    // loops or if WouldBlock errors cause multiple fn calls.
                    self.in_buffer.reserve(len);
                } else {
                    self.in_buffer.reserve(FrameHeader::MAX_SIZE);
                }
            }

            if let Some((_, len)) = &self.header {
                let len = *len as usize;
                if len <= self.in_buffer.len() {
                    break self.in_buffer.split_to(len);
                }
            }

            // Not enough data in buffer.
            if self.read_in(stream)? == 0 {
                trace!("no frame received");
                return Ok(None);
            }
        };

        let (mut header, length) = self.header.take().expect("Bug: no frame header");
        debug_assert_eq!(payload.len() as u64, length);

        if unmask {
            if let Some(mask) = header.mask.take() {
                // A server MUST remove masking for data frames received from a client
                // as described in Section 5.3. (RFC 6455)
                apply_mask(&mut payload, mask);
            } else if !accept_unmasked {
                // The server MUST close the connection upon receiving a
                // frame that is not masked. (RFC 6455)
                // The only exception here is if the user explicitly accepts given
                // stream by setting WebSocketConfig.accept_unmasked_frames to true
                return Err(Error::Protocol(ProtocolError::UnmaskedFrameFromClient));
            }
        }

        let frame = Frame::from_payload(header, payload.freeze());
        trace!("received frame {frame}");
        Ok(Some(frame))
    }

    /// Read into available `in_buffer` capacity.
    fn read_in(&mut self, stream: &mut impl Read) -> io::Result<usize> {
        let len = self.in_buffer.len();
        debug_assert!(self.in_buffer.capacity() > len);
        self.in_buffer.resize(self.in_buffer.capacity().min(len + self.in_buf_max_read), 0);
        let size = stream.read(&mut self.in_buffer[len..]);
        self.in_buffer.truncate(len + size.as_ref().copied().unwrap_or(0));
        size
    }

    /// Writes a frame into the `out_buffer`.
    /// If the out buffer size is over the `out_buffer_write_len` will also write
    /// the out buffer into the provided `stream`.
    ///
    /// To ensure buffered frames are written call [`Self::write_out_buffer`].
    ///
    /// May write to the stream, will **not** flush.
    pub(super) fn buffer_frame<Stream>(&mut self, stream: &mut Stream, frame: Frame) -> Result<()>
    where
        Stream: Write,
    {
        if frame.len() + self.out_buffer.len() > self.max_out_buffer_len {
            return Err(Error::WriteBufferFull(Message::Frame(frame).into()));
        }

        trace!("writing frame {frame}");

        self.out_buffer.reserve(frame.len());
        frame.format_into_buf(&mut self.out_buffer).expect("Bug: can't write to vector");

        if self.out_buffer.len() > self.out_buffer_write_len {
            self.write_out_buffer(stream)
        } else {
            Ok(())
        }
    }

    /// Writes the out_buffer to the provided stream.
    ///
    /// Does **not** flush.
    pub(super) fn write_out_buffer<Stream>(&mut self, stream: &mut Stream) -> Result<()>
    where
        Stream: Write,
    {
        while !self.out_buffer.is_empty() {
            let len = stream.write(&self.out_buffer)?;
            if len == 0 {
                // This is the same as "Connection reset by peer"
                return Err(IoError::new(
                    IoErrorKind::ConnectionReset,
                    "Connection reset while sending",
                )
                .into());
            }
            self.out_buffer.drain(0..len);
        }

        self.shrink_empty_out_buffer();

        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use crate::error::{CapacityError, Error};
    use crate::protocol::frame::coding::{Data, OpCode};

    use super::{Frame, FrameCodec, FrameSocket};

    use std::io::{self, Cursor, Write};

    #[derive(Debug)]
    struct PartialWouldBlockWriter {
        bytes: Vec<u8>,
        first_write_limit: usize,
        write_calls: usize,
        blocked: bool,
    }

    impl PartialWouldBlockWriter {
        fn new(first_write_limit: usize) -> Self {
            Self {
                bytes: Vec::new(),
                first_write_limit,
                write_calls: 0,
                blocked: false,
            }
        }
    }

    impl Write for PartialWouldBlockWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.blocked {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "write blocked"));
            }
            let len = if self.write_calls == 0 {
                bytes.len().min(self.first_write_limit)
            } else {
                bytes.len()
            };
            self.bytes.extend_from_slice(&bytes[..len]);
            self.write_calls += 1;
            if self.write_calls == 1 {
                self.blocked = true;
            }
            Ok(len)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn read_frames() {
        let raw = Cursor::new(vec![
            0x82, 0x07, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x82, 0x03, 0x03, 0x02, 0x01,
            0x99,
        ]);
        let mut sock = FrameSocket::new(raw);

        assert_eq!(
            sock.read(None).unwrap().unwrap().into_payload(),
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07][..]
        );
        assert_eq!(sock.read(None).unwrap().unwrap().into_payload(), &[0x03, 0x02, 0x01][..]);
        assert!(sock.read(None).unwrap().is_none());

        let (_, rest) = sock.into_inner();
        assert_eq!(rest, vec![0x99]);
    }

    #[test]
    fn from_partially_read() {
        let raw = Cursor::new(vec![0x02, 0x03, 0x04, 0x05, 0x06, 0x07]);
        let mut sock = FrameSocket::from_partially_read(raw, vec![0x82, 0x07, 0x01]);
        assert_eq!(
            sock.read(None).unwrap().unwrap().into_payload(),
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07][..]
        );
    }

    #[test]
    fn write_frames() {
        let mut sock = FrameSocket::new(Vec::new());

        let frame = Frame::ping(vec![0x04, 0x05]);
        sock.send(frame).unwrap();

        let frame = Frame::pong(vec![0x01]);
        sock.send(frame).unwrap();

        let (buf, _) = sock.into_inner();
        assert_eq!(buf, vec![0x89, 0x02, 0x04, 0x05, 0x8a, 0x01, 0x01]);
    }

    #[test]
    fn successful_drain_releases_oversized_write_buffer_capacity() {
        const WRITE_BUFFER_SIZE: usize = 128 * 1024;
        const MAX_RETAINED_CAPACITY: usize = 256 * 1024;
        let mut codec = FrameCodec::new(1024);
        codec.set_out_buffer_write_len(WRITE_BUFFER_SIZE);
        codec.set_max_retained_out_buffer_capacity(MAX_RETAINED_CAPACITY);
        let frame = Frame::message(vec![0x5a; 1024 * 1024], OpCode::Data(Data::Binary), true);
        let mut writer = Vec::new();

        codec
            .buffer_frame(&mut writer, frame)
            .expect("large frame should drain");

        assert!(codec.out_buffer.is_empty());
        assert!(codec.out_buffer.capacity() <= MAX_RETAINED_CAPACITY);
        assert_eq!(writer.len(), 1024 * 1024 + 10);
    }

    #[test]
    fn retention_policy_does_not_change_emitted_frame_bytes() {
        const WRITE_BUFFER_SIZE: usize = 128 * 1024;
        const MAX_RETAINED_CAPACITY: usize = 256 * 1024;
        let frame = Frame::compressed_message(
            bytes::Bytes::from(vec![0x8d; 1024 * 1024]),
            Data::Text,
            true,
        );
        let mut upstream_codec = FrameCodec::new(1024);
        upstream_codec.set_out_buffer_write_len(WRITE_BUFFER_SIZE);
        let mut retained_codec = FrameCodec::new(1024);
        retained_codec.set_out_buffer_write_len(WRITE_BUFFER_SIZE);
        retained_codec.set_max_retained_out_buffer_capacity(MAX_RETAINED_CAPACITY);
        let mut upstream_wire = Vec::new();
        let mut retained_wire = Vec::new();

        upstream_codec
            .buffer_frame(&mut upstream_wire, frame.clone())
            .expect("upstream frame should drain");
        retained_codec
            .buffer_frame(&mut retained_wire, frame)
            .expect("retention-limited frame should drain");

        assert_eq!(retained_wire, upstream_wire);
        assert!(retained_codec.out_buffer.capacity() <= MAX_RETAINED_CAPACITY);
        assert!(upstream_codec.out_buffer.capacity() > MAX_RETAINED_CAPACITY);
    }

    #[test]
    fn partial_would_block_keeps_bytes_and_shrinks_only_after_retry_drains() {
        const MAX_RETAINED_CAPACITY: usize = 256;
        let frame = Frame::message(vec![0x3c; 1024 * 1024], OpCode::Data(Data::Binary), true);
        let mut expected = Vec::new();
        frame
            .clone()
            .format_into_buf(&mut expected)
            .expect("frame should format");
        let mut codec = FrameCodec::new(1024);
        codec.set_out_buffer_write_len(0);
        codec.set_max_retained_out_buffer_capacity(MAX_RETAINED_CAPACITY);
        let mut writer = PartialWouldBlockWriter::new(73);

        let error = codec
            .buffer_frame(&mut writer, frame)
            .expect_err("second write must report WouldBlock");
        assert!(matches!(error, Error::Io(ref error) if error.kind() == io::ErrorKind::WouldBlock));
        assert_eq!(writer.bytes, expected[..73]);
        assert_eq!(codec.out_buffer.len(), expected.len() - 73);
        assert!(codec.out_buffer.capacity() > MAX_RETAINED_CAPACITY);

        writer.blocked = false;
        codec
            .write_out_buffer(&mut writer)
            .expect("retry should drain queued bytes");

        assert_eq!(writer.bytes, expected);
        assert!(codec.out_buffer.is_empty());
        assert!(codec.out_buffer.capacity() <= MAX_RETAINED_CAPACITY);
    }

    #[test]
    fn parse_overflow() {
        let raw = Cursor::new(vec![
            0x83, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00,
        ]);
        let mut sock = FrameSocket::new(raw);
        let _ = sock.read(None); // should not crash
    }

    #[test]
    fn size_limit_hit() {
        let raw = Cursor::new(vec![0x82, 0x07, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]);
        let mut sock = FrameSocket::new(raw);
        assert!(matches!(
            sock.read(Some(5)),
            Err(Error::Capacity(CapacityError::MessageTooLong { size: 7, max_size: 5 }))
        ));
    }
}
