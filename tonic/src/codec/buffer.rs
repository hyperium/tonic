use bytes::{Buf, Bytes};
use std::collections::VecDeque;
use std::fmt;
use tracing::warn;

pub(crate) struct BufList {
    bufs: VecDeque<Bytes>,
}

/// A buffer to decode messages from.
pub struct DecodeBuf<'a> {
    buf: &'a mut dyn Buf,
    len: usize,
}

impl<'a> DecodeBuf<'a> {
    pub(crate) fn new(buf: &'a mut dyn Buf, len: usize) -> Self {
        DecodeBuf { buf, len }
    }
}

impl BufList {
    pub(crate) fn new() -> Self {
        BufList {
            bufs: VecDeque::new(),
        }
    }

    pub(crate) fn push(&mut self, bytes: Bytes) {
        debug_assert!(bytes.has_remaining());
        self.bufs.push_back(bytes)
    }
}

impl Buf for BufList {
    #[inline]
    fn remaining(&self) -> usize {
        self.bufs.iter().fold(0, |a, b| a + b.remaining())
    }

    #[inline]
    fn bytes(&self) -> &[u8] {
        self.bufs.front().map(Buf::bytes).unwrap_or_default()
    }

    #[inline]
    fn advance(&mut self, mut cnt: usize) {
        while cnt > 0 {
            {
                let front = &mut self.bufs[0];
                let rem = front.remaining();
                if rem > cnt {
                    front.advance(cnt);
                    return;
                } else {
                    front.advance(rem);
                    cnt -= rem;
                }
            }
            self.bufs.pop_front();
        }
    }
}

impl<'a> Buf for DecodeBuf<'a> {
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

impl<'a> fmt::Debug for DecodeBuf<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodeBuf").finish()
    }
}

impl<'a> Drop for DecodeBuf<'a> {
    fn drop(&mut self) {
        if self.len > 0 {
            warn!("DecodeBuf was not advanced to end");
            self.buf.advance(self.len);
        }
    }
}
