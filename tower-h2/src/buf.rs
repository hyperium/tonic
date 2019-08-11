use bytes::Buf;

pub struct SendBuf<T> {
    inner: Option<T>,
}

impl<T: Buf> SendBuf<T> {
    pub fn new(buf: T) -> SendBuf<T> {
        SendBuf { inner: Some(buf) }
    }

    pub fn none() -> SendBuf<T> {
        SendBuf { inner: None }
    }
}

impl<T: Buf> Buf for SendBuf<T> {
    fn remaining(&self) -> usize {
        match self.inner {
            Some(ref v) => v.remaining(),
            None => 0,
        }
    }

    fn bytes(&self) -> &[u8] {
        match self.inner {
            Some(ref v) => v.bytes(),
            None => &[],
        }
    }

    fn advance(&mut self, cnt: usize) {
        match self.inner {
            Some(ref mut v) => v.advance(cnt),
            None => {}
        }
    }
}
