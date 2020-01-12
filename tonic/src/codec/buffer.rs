use bytes::{Buf, BufMut, BytesMut};
use std::mem::MaybeUninit;

/// A buffer to decode messages from.
#[derive(Debug)]
pub struct DecodeBuf<'a> {
    buf: &'a mut BytesMut,
    len: usize,
}

/// A buffer to encode a message into.
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
    #[doc(hidden)]
    #[inline]
    pub fn reserve(&mut self, capacity: usize) {
        self.buf.reserve(capacity);
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
