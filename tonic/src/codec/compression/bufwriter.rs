use bytes::BufMut;

use std::{cmp, io};

/// A `BufMut` adapter which implements `io::Write` for the inner value.
#[derive(Debug)]
pub(crate) struct Writer<'a, B> {
    buf: &'a mut B,
}

pub(crate) fn new<'a, B>(buf: &'a mut B) -> Writer<'a, B> {
    Writer { buf }
}

impl<'a, B: BufMut + Sized> io::Write for Writer<'a, B> {
    fn write(&mut self, src: &[u8]) -> io::Result<usize> {
        let n = cmp::min(self.buf.remaining_mut(), src.len());

        self.buf.put(&src[0..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
