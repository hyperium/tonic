use bytes::{Buf, BufMut, BytesMut};
use std::mem::MaybeUninit;

/// A specialized buffer to decode gRPC messages from.
#[derive(Debug)]
pub struct DecodeBuf<'a> {
    buf: &'a mut BytesMut,
    len: usize,
}

/// A specialized buffer to encode gRPC messages into.
#[derive(Debug)]
pub struct EncodeBuf<'a> {
    buf: &'a mut BytesMut,
}

impl<'a> DecodeBuf<'a> {
    pub(crate) fn new(buf: &'a mut BytesMut, len: usize) -> Self {
        DecodeBuf { buf, len }
    }
}

impl Buf for DecodeBuf<'_> {
    #[inline]
    fn remaining(&self) -> usize {
        self.len
    }

    #[inline]
    fn bytes(&self) -> &[u8] {
        let ret = self.buf.bytes();

        if ret.len() > self.len {
            &ret[..self.len]
        } else {
            ret
        }
    }

    #[inline]
    fn advance(&mut self, cnt: usize) {
        assert!(cnt <= self.len);
        self.buf.advance(cnt);
        self.len -= cnt;
    }
}

impl<'a> EncodeBuf<'a> {
    pub(crate) fn new(buf: &'a mut BytesMut) -> Self {
        EncodeBuf { buf }
    }
}

impl EncodeBuf<'_> {
    /// Reserves capacity for at least `additional` more bytes to be inserted
    /// into the buffer.
    ///
    /// More than `additional` bytes may be reserved in order to avoid frequent
    /// reallocations. A call to `reserve` may result in an allocation.
    #[inline]
    pub fn reserve(&mut self, additional: usize) {
        self.buf.reserve(additional);
    }
}

impl BufMut for EncodeBuf<'_> {
    #[inline]
    fn remaining_mut(&self) -> usize {
        self.buf.remaining_mut()
    }

    #[inline]
    unsafe fn advance_mut(&mut self, cnt: usize) {
        self.buf.advance_mut(cnt)
    }

    #[inline]
    fn bytes_mut(&mut self) -> &mut [MaybeUninit<u8>] {
        self.buf.bytes_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_buf() {
        let mut payload = BytesMut::with_capacity(100);
        payload.put(&vec![0u8; 50][..]);
        let mut buf = DecodeBuf::new(&mut payload, 20);

        assert_eq!(buf.len, 20);
        assert_eq!(buf.remaining(), 20);
        assert_eq!(buf.bytes().len(), 20);

        buf.advance(10);
        assert_eq!(buf.remaining(), 10);

        let mut out = [0; 5];
        buf.copy_to_slice(&mut out);
        assert_eq!(buf.remaining(), 5);
        assert_eq!(buf.bytes().len(), 5);

        assert_eq!(buf.to_bytes().len(), 5);
        assert!(!buf.has_remaining());
    }

    #[test]
    fn encode_buf() {
        let mut bytes = BytesMut::with_capacity(100);
        let mut buf = EncodeBuf::new(&mut bytes);

        let initial = buf.remaining_mut();
        unsafe { buf.advance_mut(20) };
        assert_eq!(buf.remaining_mut(), initial - 20);

        buf.put_u8(b'a');
        assert_eq!(buf.remaining_mut(), initial - 20 - 1);
    }
}
