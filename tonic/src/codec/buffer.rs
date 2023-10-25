use bytes::{buf::UninitSlice, Buf, BufMut, Bytes, BytesMut};
use std::{cmp, collections::VecDeque, io, iter, ops::Deref};

/// A specialized buffer to decode gRPC messages from.
#[derive(Debug)]
pub struct DecodeBuf<'a> {
    buf: &'a mut SliceBuffer,
    len: usize,
}

/// A specialized buffer to encode gRPC messages into.
#[derive(Debug)]
pub struct EncodeBuf<'a> {
    buf: &'a mut SliceBuffer,
}

impl<'a> DecodeBuf<'a> {
    pub(crate) fn new(buf: &'a mut SliceBuffer, len: usize) -> Self {
        DecodeBuf { buf, len }
    }
}

impl Buf for DecodeBuf<'_> {
    #[inline]
    fn remaining(&self) -> usize {
        self.len
    }

    #[inline]
    fn chunk(&self) -> &[u8] {
        let ret = self.buf.chunk();

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

    #[inline]
    fn copy_to_bytes(&mut self, len: usize) -> Bytes {
        assert!(len <= self.len);
        self.len -= len;
        self.buf.copy_to_bytes(len)
    }
}

impl<'a> EncodeBuf<'a> {
    pub(crate) fn new(buf: &'a mut SliceBuffer) -> Self {
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

    /// Inserts a byte slice directly into the underlying [`SliceBuffer`] without copying.
    /// Instead of copying the data to the active buffer, it appends to the collection
    /// of slices within the [`SliceBuffer`]. This operation completes in constant time,
    /// provided no memory reallocation occurs.
    #[inline]
    pub fn insert_slice(&mut self, slice: Bytes) {
        self.buf.insert_slice(slice)
    }
}

unsafe impl BufMut for EncodeBuf<'_> {
    #[inline]
    fn remaining_mut(&self) -> usize {
        self.buf.remaining_mut()
    }

    #[inline]
    unsafe fn advance_mut(&mut self, cnt: usize) {
        self.buf.advance_mut(cnt)
    }

    #[inline]
    fn chunk_mut(&mut self) -> &mut UninitSlice {
        self.buf.chunk_mut()
    }

    #[inline]
    fn put<T: Buf>(&mut self, src: T)
    where
        Self: Sized,
    {
        self.buf.put(src)
    }

    #[inline]
    fn put_slice(&mut self, src: &[u8]) {
        self.buf.put_slice(src)
    }

    #[inline]
    fn put_bytes(&mut self, val: u8, cnt: usize) {
        self.buf.put_bytes(val, cnt)
    }
}

#[derive(Debug, PartialEq)]
enum Slice {
    Fixed(Bytes),
    Mutable(BytesMut),
}

impl Slice {
    #[inline]
    fn len(&self) -> usize {
        match self {
            Slice::Fixed(buf) => buf.len(),
            Slice::Mutable(buf) => buf.len(),
        }
    }

    #[inline]
    fn split_to(&mut self, at: usize) -> Self {
        match self {
            Slice::Fixed(buf) => Slice::Fixed(buf.split_to(at)),
            Slice::Mutable(buf) => Slice::Mutable(buf.split_to(at)),
        }
    }
}

impl Default for Slice {
    fn default() -> Self {
        Slice::Fixed(Default::default())
    }
}

impl Buf for Slice {
    #[inline]
    fn remaining(&self) -> usize {
        match self {
            Slice::Fixed(buf) => buf.remaining(),
            Slice::Mutable(buf) => buf.remaining(),
        }
    }

    #[inline]
    fn chunk(&self) -> &[u8] {
        match self {
            Slice::Fixed(buf) => buf.chunk(),
            Slice::Mutable(buf) => buf.chunk(),
        }
    }

    #[inline]
    fn advance(&mut self, cnt: usize) {
        match self {
            Slice::Fixed(buf) => buf.advance(cnt),
            Slice::Mutable(buf) => buf.advance(cnt),
        }
    }

    #[inline]
    fn copy_to_bytes(&mut self, len: usize) -> Bytes {
        match self {
            Slice::Fixed(buf) => buf.copy_to_bytes(len),
            Slice::Mutable(buf) => buf.copy_to_bytes(len),
        }
    }
}

impl Deref for Slice {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        match self {
            Slice::Fixed(buf) => buf.deref(),
            Slice::Mutable(buf) => buf.deref(),
        }
    }
}

/// `SliceBuffer` represents a buffer containing non-contiguous memory segments, which implements
/// [`bytes::Buf`] and [`bytes::BufMut`]. While traditional buffers typically rely on contiguous
/// memory, `SliceBuffer` offers a unique design allowing the seamless insertion of immutable byte
/// chunks without necessitating memory copying or potential memory reallocation.
///
/// Internally, `SliceBuffer` consists of two main components:
/// 1. A collection of slices.
/// 2. An active buffer.
///
/// Together, these components present the bytes as a concatenated sequence of all the slices and
/// the active buffer. The active buffer behaves similarly to [`bytes::BytesMut`], copying inserted
/// bytes into it. For larger slices represented as [`bytes::Bytes`], they can be directly appended
/// to the slice collection, bypassing data copying.
#[derive(Default, Debug)]
pub struct SliceBuffer {
    active: BytesMut,
    len: usize,
    slices: VecDeque<Slice>,
}

impl SliceBuffer {
    /// Constructs a new `SliceBuffer` with given capacities for both the slice collection and the
    /// active buffer. The created `SliceBuffer` ensures the slice collection can accommodate
    /// at least `slice_capacity` slices, and the active buffer can contain at least
    /// `buffer_capacity` bytes.
    ///
    /// Note: This function determines the capacity, not the length, of the returned `SliceBuffer`.
    #[inline]
    pub fn with_capacity(slice_capacity: usize, buffer_capacity: usize) -> Self {
        SliceBuffer {
            active: match buffer_capacity {
                0 => BytesMut::new(),
                _ => BytesMut::with_capacity(buffer_capacity),
            },
            slices: match slice_capacity {
                0 => VecDeque::new(),
                _ => VecDeque::with_capacity(slice_capacity),
            },
            ..Default::default()
        }
    }

    /// Divides the `SliceBuffer` into two at the specified index.
    ///
    /// Following the split, `self` retains elements from `[at, len)`, while the returned
    /// `SliceBuffer` encapsulates elements from `[0, at)`. This operation has a time complexity
    /// of `O(N)`, where `N` denotes the number of slices in the `SliceBuffer`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bytes::{Bytes, Buf};
    /// use tonic::codec::SliceBuffer;
    ///
    /// let mut buf1 = SliceBuffer::from(Bytes::from(&b"hello world"[..]));
    /// let mut buf2 = buf1.split_to(5);
    ///
    /// assert_eq!(buf1.copy_to_bytes(buf1.len()), Bytes::from(&b" world"[..]));
    /// assert_eq!(buf2.copy_to_bytes(buf2.len()), Bytes::from(&b"hello"[..]));
    /// ```
    ///
    /// # Panics
    ///
    /// This method will panic if `at` exceeds `len`.
    pub fn split_to(&mut self, at: usize) -> Self {
        assert!(
            at <= self.len(),
            "split_to out of bounds: {:?} <= {:?}",
            at,
            self.len(),
        );

        self.len -= at;
        let at_pos = self.cursor().forward(&Position::default(), at);
        let mut buf = SliceBuffer {
            active: Default::default(),
            len: at,
            slices: VecDeque::with_capacity(at_pos.slice_idx + cmp::min(at_pos.rel_pos, 1)),
        };
        buf.slices.extend(self.slices.drain(..at_pos.slice_idx));

        if at_pos.rel_pos > 0 {
            let slice = if let Some(slice) = self.slices.front_mut() {
                slice.split_to(at_pos.rel_pos)
            } else {
                Slice::Mutable(self.active.split_to(at_pos.rel_pos))
            };
            buf.slices.push_back(slice);
        }
        buf
    }

    /// Transfers all elements from `other` to `self`, emptying `other` in the process. Slices from
    /// `other` are appended to `self`'s slice collection, while `other`'s active buffer is copied
    /// over to `self`'s active buffer.
    ///
    /// # Examples
    ///
    /// ```
    /// use bytes::{Buf, BufMut, Bytes};
    /// use tonic::codec::SliceBuffer;
    ///
    /// let mut buf1 = SliceBuffer::default();
    /// buf1.put_slice(b"foo");
    /// let mut buf2 = SliceBuffer::default();
    /// buf2.insert_slice(Bytes::copy_from_slice(b"bar"));
    /// buf2.put(Bytes::copy_from_slice(b"foo"));
    /// buf1.append(&mut buf2);
    /// assert_eq!(buf1.copy_to_bytes(buf1.len()), Bytes::copy_from_slice(b"foobarfoo"));
    /// assert_eq!(buf2.len(), 0);
    /// ```
    ///
    /// # Panics
    ///
    /// This will panic if the total number of elements in `self` exceeds the maximum `usize` value.
    pub fn append(&mut self, other: &mut Self) {
        if other.slices.len() > 0 {
            self.slices
                .reserve(other.slices.len() + cmp::min(1, self.active.len()));
            if self.active.has_remaining() {
                self.slices
                    .push_back(Slice::Mutable(self.active.split_to(self.active.len())));
            }
            self.slices.append(&mut other.slices);
        }
        self.active.put(other.active.split_to(other.active.len()));
        self.len += other.len;
        other.len = 0;
    }

    /// Returns the number of bytes contained in this `SliceBuffer`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bytes::Bytes;
    /// use tonic::codec::SliceBuffer;
    ///
    /// let b = SliceBuffer::from(Bytes::copy_from_slice(b"hello world"));
    /// assert_eq!(b.len(), 11);
    /// ```
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns true if the `SliceBuffer` has a length of 0.
    ///
    /// # Examples
    ///
    /// ```
    /// use tonic::codec::SliceBuffer;
    ///
    /// let b = SliceBuffer::default();
    /// assert!(b.is_empty());
    /// ```
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reserves space for an additional `additional` bytes in the active buffer
    /// of the `SliceBuffer`.
    ///
    /// This directly invokes the `reserve` method on the active buffer, which is of type
    /// [`bytes::BytesMut`]. Refer to [`bytes::BytesMut::reserve`] for further details.
    ///
    /// # Panics
    ///
    /// This will panic if the new capacity exceeds the maximum `usize` value.
    #[inline]
    pub fn reserve(&mut self, additional: usize) {
        self.active.reserve(additional);
    }

    /// Inserts an immutable slice, represented by [`bytes::Bytes`], into the `SliceBuffer`.
    ///
    /// By directly adding the slice to the `SliceBuffer`'s slice collection, this operation
    /// bypasses any memory copying. If there's existing content in the active buffer, it will be
    /// transferred to the slice collection as a mutable slice prior to the insertion.
    ///
    /// # Examples
    ///
    /// ```
    /// use bytes::{BufMut, Bytes, Buf};
    /// use tonic::codec::SliceBuffer;
    ///
    /// let mut buf = SliceBuffer::with_capacity(128, 1024);
    /// buf.put_slice(b"foo");
    /// buf.insert_slice(Bytes::copy_from_slice(b"bar"));
    /// assert_eq!(buf.copy_to_bytes(buf.len()), Bytes::copy_from_slice(b"foobar"));
    /// ```
    #[inline]
    pub fn insert_slice(&mut self, slice: Bytes) {
        if slice.len() > 0 {
            if !self.active.is_empty() {
                self.slices
                    .push_back(Slice::Mutable(self.active.split_to(self.active.len())));
            }
            self.len += slice.len();
            self.slices.push_back(Slice::Fixed(slice));
        }
    }

    /// Clears the `SliceBuffer`, removing all data. Existing capacity is preserved.
    ///
    /// # Examples
    ///
    /// ```
    /// use bytes::Bytes;
    /// use tonic::codec::SliceBuffer;
    ///
    /// let mut buf = SliceBuffer::from(Bytes::copy_from_slice(b"hello world"));
    /// assert!(!buf.is_empty());
    /// buf.clear();
    /// assert!(buf.is_empty());
    /// ```
    #[inline]
    pub fn clear(&mut self) {
        self.active.clear();
        self.len = 0;
        self.slices.clear();
    }
}

impl Buf for SliceBuffer {
    #[inline]
    fn remaining(&self) -> usize {
        self.len()
    }

    #[inline]
    fn chunk(&self) -> &[u8] {
        match self.slices.front() {
            Some(bytes) => bytes.chunk(),
            None => self.active.chunk(),
        }
    }

    fn chunks_vectored<'a>(&'a self, dst: &mut [io::IoSlice<'a>]) -> usize {
        let mut n = 0;
        for slice in self.slices.iter() {
            n += slice.chunks_vectored(&mut dst[n..]);
        }
        n += self.active.chunks_vectored(&mut dst[n..]);
        n
    }

    #[inline]
    fn advance(&mut self, mut cnt: usize) {
        assert!(
            cnt <= self.len(),
            "cannot advance past `remaining`: {:?} <= {:?}",
            cnt,
            self.len(),
        );

        self.len -= cnt;
        while cnt > 0 {
            match self.slices.front_mut() {
                Some(slice) => {
                    if slice.len() <= cnt {
                        cnt -= slice.len();
                        self.slices.pop_front();
                    } else {
                        slice.advance(cnt);
                        cnt = 0;
                    }
                }
                None => {
                    self.active.advance(cnt);
                    cnt = 0;
                }
            }
        }
    }

    fn copy_to_bytes(&mut self, len: usize) -> Bytes {
        match self.slices.front_mut() {
            Some(slice) if slice.len() > len => {
                self.len -= len;
                slice.copy_to_bytes(len)
            }
            Some(slice) if slice.len() == len => {
                self.len -= len;
                let buf = slice.copy_to_bytes(len);
                self.slices.pop_front();
                buf
            }
            None => {
                self.len -= len;
                self.active.copy_to_bytes(len)
            }
            _ => {
                assert!(len <= self.remaining(), "`len` greater than remaining");
                let mut buf = BytesMut::with_capacity(len);
                buf.put(self.take(len));
                buf.freeze()
            }
        }
    }
}

unsafe impl BufMut for SliceBuffer {
    #[inline]
    fn remaining_mut(&self) -> usize {
        self.active.remaining_mut()
    }

    #[inline]
    unsafe fn advance_mut(&mut self, cnt: usize) {
        self.len += cnt;
        self.active.advance_mut(cnt)
    }

    #[inline]
    fn chunk_mut(&mut self) -> &mut UninitSlice {
        self.active.chunk_mut()
    }

    #[inline]
    fn put<T: Buf>(&mut self, src: T)
    where
        Self: Sized,
    {
        self.len += src.remaining();
        self.active.put(src)
    }

    #[inline]
    fn put_slice(&mut self, src: &[u8]) {
        self.len += src.len();
        self.active.put_slice(src)
    }

    #[inline]
    fn put_bytes(&mut self, val: u8, cnt: usize) {
        self.len += cnt;
        self.active.put_bytes(val, cnt)
    }
}

impl From<Bytes> for SliceBuffer {
    fn from(bytes: Bytes) -> Self {
        SliceBuffer {
            active: BytesMut::new(),
            len: bytes.len(),
            slices: VecDeque::from_iter(iter::once(Slice::Fixed(bytes))),
        }
    }
}

impl From<SliceBuffer> for Bytes {
    fn from(mut buf: SliceBuffer) -> Self {
        if cmp::min(buf.active.len(), 1) + cmp::min(buf.slices.len(), 1) > 1 {
            tracing::warn!("multiple chunks exist in the slice buffer")
        }
        buf.copy_to_bytes(buf.remaining())
    }
}

#[derive(Clone, Debug, Default)]
struct Position {
    abs_pos: usize,
    rel_pos: usize,
    slice_idx: usize,
}

impl SliceBuffer {
    /// Creates a `Cursor` tailored to the `SliceBuffer`.
    ///
    /// This `Cursor` grants mutable access to the `SliceBuffer` while keeping track of its
    /// position. By implementing [`std::io::Seek`], [`bytes::Buf`], and [`std::io::Write`],
    /// it allows random access similar to a seamless memory segment.
    ///
    /// **Note**:
    /// 1. Writing to an immutable slice: If you try to write to an immutable slice in the
    /// collection using [`std::io::Write::write`], an error will be returned.
    /// 2. Seeking beyond bounds: Attempting to move the cursor beyond the buffer's range
    /// with [`std::io::Seek::seek`] will also yield an error.
    ///
    /// # Examples
    ///
    /// ```
    /// use bytes::{Buf, BufMut, Bytes};
    /// use tonic::codec::SliceBuffer;
    /// use std::io::{Seek, SeekFrom, Write};
    ///
    /// let mut buf = SliceBuffer::default();
    /// buf.insert_slice(Bytes::copy_from_slice(b"foo"));
    /// buf.put(Bytes::copy_from_slice(b"bar"));
    ///
    /// let mut cursor = buf.cursor();
    /// cursor.seek(SeekFrom::Start(1)).unwrap();
    /// assert_eq!(cursor.copy_to_bytes(2), Bytes::copy_from_slice(b"oo"));
    ///
    /// cursor.seek(SeekFrom::Start(1)).unwrap();
    /// assert!(cursor.write(&b"b"[..]).is_err());
    ///
    /// cursor.seek(SeekFrom::Current(3)).unwrap();
    /// cursor.write(&b"b"[..]).unwrap();
    /// drop(cursor);
    /// assert_eq!(buf.copy_to_bytes(buf.len()), Bytes::copy_from_slice(b"foobbr"));
    /// ```
    pub fn cursor(&mut self) -> Cursor<'_> {
        Cursor {
            buffer: self,
            pos: Position::default(),
        }
    }

    /// Returns a front-to-back iterator over the bytes in the `SliceBuffer`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bytes::{BufMut, Bytes};
    /// use tonic::codec::SliceBuffer;
    ///
    /// let mut buf = SliceBuffer::default();
    /// buf.insert_slice(Bytes::copy_from_slice(b"foo"));
    /// buf.put(Bytes::copy_from_slice(b"bar"));
    ///
    /// let iter = buf.iter();
    /// assert_eq!(iter.collect::<Vec<u8>>(), b"foobar".to_vec());
    /// ```
    pub fn iter(&mut self) -> Iter<'_> {
        Iter {
            cursor: self.cursor(),
        }
    }
}

#[derive(Debug)]
pub struct Cursor<'a> {
    buffer: &'a mut SliceBuffer,
    pos: Position,
}

impl<'a> Cursor<'a> {
    fn forward(&self, from: &Position, mut offset: usize) -> Position {
        let mut pos = from.to_owned();
        for slice in self.buffer.slices.range(from.slice_idx..) {
            if slice.len() - pos.rel_pos <= offset {
                pos.abs_pos += slice.len() - pos.rel_pos;
                offset -= slice.len() - pos.rel_pos;
                pos.rel_pos = 0;
                pos.slice_idx += 1;
            } else {
                pos.abs_pos += offset;
                pos.rel_pos += offset;
                return pos;
            }
        }
        let curr_pos = cmp::min(self.buffer.active.len(), pos.rel_pos + offset);
        pos.abs_pos += curr_pos - pos.rel_pos;
        pos.rel_pos = curr_pos;
        pos
    }

    fn backward(&self, from: &Position, mut offset: usize) -> Position {
        let mut pos = from.to_owned();
        if offset <= pos.rel_pos {
            pos.abs_pos -= offset;
            pos.rel_pos -= offset;
            return pos;
        } else {
            pos.abs_pos -= pos.rel_pos;
            offset -= pos.rel_pos;
            pos.rel_pos = 0;
        }
        for slice in self.buffer.slices.range(0..from.slice_idx).rev() {
            pos.slice_idx -= 1;
            if offset <= slice.len() {
                pos.abs_pos -= offset;
                pos.rel_pos = slice.len() - offset;
                return pos;
            } else {
                pos.abs_pos -= slice.len();
                offset -= slice.len();
            }
        }
        Position::default()
    }

    fn write_once(&mut self, buf: &[u8]) -> io::Result<usize> {
        use io::{Seek, Write};

        let n = if let Some(slice) = self.buffer.slices.get_mut(self.pos.slice_idx) {
            match slice {
                Slice::Mutable(slice) => slice.split_at_mut(self.pos.rel_pos).1.write(buf)?,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "cannot write to immutable slice",
                    ));
                }
            }
        } else if self.pos.rel_pos < self.buffer.active.len() {
            self.buffer
                .active
                .split_at_mut(self.pos.rel_pos)
                .1
                .write(buf)?
        } else {
            self.buffer.active.extend_from_slice(buf);
            self.buffer.len += buf.len();
            buf.len()
        };
        self.seek(io::SeekFrom::Current(n as i64))?;
        Ok(n)
    }
}

impl<'a> io::Seek for Cursor<'a> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        let (base_pos, offset) = match pos {
            io::SeekFrom::Start(offset) => (Position::default(), offset as i64),
            io::SeekFrom::End(offset) => (
                Position {
                    abs_pos: self.buffer.len(),
                    rel_pos: self.buffer.active.len(),
                    slice_idx: self.buffer.slices.len(),
                },
                offset,
            ),
            io::SeekFrom::Current(offset) => (self.pos.clone(), offset),
        };

        if base_pos
            .abs_pos
            .checked_add_signed(offset as isize)
            .map(|pos| pos <= self.buffer.len())
            .unwrap_or_default()
        {
            self.pos = if offset >= 0 {
                self.forward(&base_pos, offset as usize)
            } else {
                self.backward(&base_pos, offset.abs() as usize)
            };
            Ok(self.pos.abs_pos as u64)
        } else {
            Err(io::Error::new(io::ErrorKind::InvalidInput, "out of range"))
        }
    }
}

impl<'a> io::Write for Cursor<'a> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            n += self.write_once(&buf[n..])?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> Buf for Cursor<'a> {
    #[inline]
    fn remaining(&self) -> usize {
        self.buffer.len() - self.pos.abs_pos
    }

    #[inline]
    fn chunk(&self) -> &[u8] {
        let slice = match self.buffer.slices.get(self.pos.slice_idx) {
            Some(bytes) => bytes.chunk(),
            None => &self.buffer.active.chunk(),
        };
        &slice[self.pos.rel_pos..]
    }

    fn advance(&mut self, cnt: usize) {
        assert!(
            cnt <= self.remaining(),
            "cannot advance past `remaining`: {:?} <= {:?}",
            cnt,
            self.remaining(),
        );
        self.pos = self.forward(&self.pos, cnt);
    }
}

#[derive(Debug)]
pub struct Iter<'a> {
    cursor: Cursor<'a>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        use io::Seek;

        if self.cursor.has_remaining() {
            let item = if let Some(slice) = self.cursor.buffer.slices.get(self.cursor.pos.slice_idx)
            {
                slice[self.cursor.pos.rel_pos]
            } else {
                self.cursor.buffer.active[self.cursor.pos.rel_pos]
            };
            self.cursor.seek(io::SeekFrom::Current(1)).ok()?;
            Some(item)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, Write};

    #[test]
    fn decode_buf() {
        let mut payload = SliceBuffer::with_capacity(128, 1024);
        payload.put(&vec![0u8; 50][..]);
        let mut buf = DecodeBuf::new(&mut payload, 20);

        assert_eq!(buf.len, 20);
        assert_eq!(buf.remaining(), 20);
        assert_eq!(buf.chunk().len(), 20);

        buf.advance(10);
        assert_eq!(buf.remaining(), 10);

        let mut out = [0; 5];
        buf.copy_to_slice(&mut out);
        assert_eq!(buf.remaining(), 5);
        assert_eq!(buf.chunk().len(), 5);

        assert_eq!(buf.copy_to_bytes(5).len(), 5);
        assert!(!buf.has_remaining());
    }

    #[test]
    fn encode_buf() {
        let mut bytes = SliceBuffer::with_capacity(128, 1024);
        let mut buf = EncodeBuf::new(&mut bytes);

        let initial = buf.remaining_mut();
        unsafe { buf.advance_mut(20) };
        assert_eq!(buf.remaining_mut(), initial - 20);

        buf.put_u8(b'a');
        assert_eq!(buf.remaining_mut(), initial - 20 - 1);
    }

    #[test]
    fn sequential_read() {
        let mut buf = SliceBuffer::with_capacity(128, 1024);
        buf.put_slice(b"foo");
        buf.insert_slice(Bytes::copy_from_slice(b"bar"));
        buf.put_slice(b"foofoo");
        buf.insert_slice(Bytes::copy_from_slice(b"barbar"));
        buf.put_slice(b"foobar");
        assert_eq!(24, buf.remaining());
        assert_eq!(Bytes::copy_from_slice(b"foob"), buf.copy_to_bytes(4));
        assert_eq!(Bytes::copy_from_slice(b"arfoo"), buf.copy_to_bytes(5));
        assert_eq!(Bytes::copy_from_slice(b"foobar"), buf.copy_to_bytes(6));

        let mut buf = buf.split_to(buf.remaining());
        assert_eq!(Bytes::copy_from_slice(b"barfoo"), buf.copy_to_bytes(6));
        assert_eq!(Bytes::copy_from_slice(b"bar"), buf.copy_to_bytes(3));
        assert_eq!(0, buf.remaining());
    }

    #[test]
    fn sequential_vectored_read() {
        let mut buf = SliceBuffer::with_capacity(128, 1024);
        buf.put_slice(b"foo");
        buf.insert_slice(Bytes::copy_from_slice(b"bar"));
        buf.put_slice(b"foobar");

        let mut iovs = [io::IoSlice::new(&[]); 2];
        assert_eq!(2, buf.chunks_vectored(&mut iovs));
        assert_eq!(b"foo", iovs[0].as_ref());
        assert_eq!(b"bar", iovs[1].as_ref());
    }

    #[test]
    fn random_read() {
        let mut buf = SliceBuffer::with_capacity(128, 1024);
        buf.put_slice(b"foo");
        buf.insert_slice(Bytes::copy_from_slice(b"bar"));
        buf.put_slice(b"foofoo");
        buf.insert_slice(Bytes::copy_from_slice(b"barbar"));
        buf.put_slice(b"foobar");
        assert_eq!(24, buf.remaining());
        {
            let mut cursor = buf.cursor();
            cursor.seek(io::SeekFrom::Current(5)).unwrap();
            assert_eq!(Bytes::copy_from_slice(b"rfo"), cursor.copy_to_bytes(3));
            cursor.seek(io::SeekFrom::Current(5)).unwrap();
            assert_eq!(Bytes::copy_from_slice(b"arb"), cursor.copy_to_bytes(3));
            assert_eq!(b'a', cursor.get_u8());
        }
        assert_eq!(24, buf.remaining());
    }

    #[test]
    fn split_to() {
        let mut buf = SliceBuffer::with_capacity(128, 1024);
        buf.put_slice(b"foo");
        buf.insert_slice(Bytes::copy_from_slice(b"bar"));
        buf.put_slice(b"foofoo");
        buf.insert_slice(Bytes::copy_from_slice(b"barbar"));
        buf.put_slice(b"foobar");
        assert_eq!(24, buf.remaining());
        assert_eq!(
            Bytes::copy_from_slice(b"foo"),
            buf.split_to(3).copy_to_bytes(3)
        );
        assert_eq!(
            Bytes::copy_from_slice(b"barfoo"),
            buf.split_to(6).copy_to_bytes(6)
        );

        let split_buf = buf.split_to(6);
        assert_eq!(2, split_buf.slices.len());
        assert_eq!(
            Slice::Mutable(BytesMut::from(b"foo".as_ref())),
            split_buf.slices[0]
        );
        assert_eq!(
            Slice::Fixed(Bytes::copy_from_slice(b"bar")),
            split_buf.slices[1]
        );

        let split_buf = buf.split_to(6);
        assert_eq!(2, split_buf.slices.len());
        assert_eq!(
            Slice::Fixed(Bytes::copy_from_slice(b"bar")),
            split_buf.slices[0]
        );
        assert_eq!(
            Slice::Mutable(BytesMut::from(b"foo".as_ref())),
            split_buf.slices[1]
        );
        assert_eq!(3, buf.len());
        assert_eq!(0, buf.slices.len());
        assert_eq!(Bytes::copy_from_slice(b"bar"), buf.active.freeze());
    }

    #[test]
    fn append() {
        let mut buf = SliceBuffer::with_capacity(128, 1024);
        buf.put_slice(b"foo");
        buf.insert_slice(Bytes::copy_from_slice(b"bar"));
        buf.put_slice(b"foofoo");
        let mut buf2 = SliceBuffer::with_capacity(128, 1024);
        buf2.insert_slice(Bytes::copy_from_slice(b"barbar"));
        buf2.put_slice(b"foobar");
        buf.append(&mut buf2);

        assert_eq!(24, buf.len());
        assert_eq!(4, buf.slices.len());
        assert_eq!(
            Slice::Mutable(BytesMut::from(b"foo".as_ref())),
            buf.slices[0]
        );
        assert_eq!(Slice::Fixed(Bytes::copy_from_slice(b"bar")), buf.slices[1]);
        assert_eq!(
            Slice::Mutable(BytesMut::from(b"foofoo".as_ref())),
            buf.slices[2]
        );
        assert_eq!(
            Slice::Fixed(Bytes::copy_from_slice(b"barbar")),
            buf.slices[3]
        );
        assert_eq!(BytesMut::from(b"foobar".as_ref()), buf.active);

        assert_eq!(0, buf2.len());
        assert!(buf2.slices.is_empty());
        assert!(buf2.active.is_empty());
    }

    macro_rules! random_write_test {
        (
            name: $name:ident,
            start_pos: $start_pos:expr,
            seek: $seek:expr,
            buf: $buf:expr,
            expect: $expect:expr,
        ) => {
            #[test]
            fn $name() {
                let mut buf = SliceBuffer::with_capacity(128, 1024);
                buf.put_slice(b"foo");
                buf.insert_slice(Bytes::copy_from_slice(b"bar"));
                buf.put_slice(b"foobar");
                let mut cursor = buf.cursor();
                cursor.pos = $start_pos;
                cursor.seek($seek).unwrap();
                if let Some(expect) = $expect {
                    cursor.write($buf).unwrap();
                    assert_eq!(
                        Bytes::copy_from_slice(expect),
                        buf.copy_to_bytes(buf.remaining())
                    );
                } else {
                    assert!(cursor.write($buf).is_err());
                }
            }
        };
    }

    random_write_test!(
        name: seek_from_start_1,
        start_pos: Position::default(),
        seek: io::SeekFrom::Start(1),
        buf: b"fo",
        expect: Some(b"ffobarfoobar"),
    );

    random_write_test!(
        name: seek_from_start_2,
        start_pos: Position::default(),
        seek: io::SeekFrom::Start(1),
        buf: b"fo",
        expect: Some(b"ffobarfoobar"),
    );

    random_write_test!(
        name: seek_from_start_3,
        start_pos: Position::default(),
        seek: io::SeekFrom::Start(5),
        buf: b"foo",
        expect: None,
    );

    random_write_test!(
        name: seek_from_start_4,
        start_pos: Position::default(),
        seek: io::SeekFrom::Start(11),
        buf: b"foo",
        expect: Some(b"foobarfoobafoo"),
    );

    random_write_test!(
        name: seek_from_end_1,
        start_pos: Position::default(),
        seek: io::SeekFrom::End(-2),
        buf: b"foo",
        expect: Some(b"foobarfoobfoo"),
    );

    random_write_test!(
        name: seek_from_end_2,
        start_pos: Position::default(),
        seek: io::SeekFrom::End(-5),
        buf: b"foo",
        expect: Some(b"foobarffooar"),
    );

    random_write_test!(
        name: seek_from_end_3,
        start_pos: Position::default(),
        seek: io::SeekFrom::End(-11),
        buf: b"foo",
        expect: None,
    );

    random_write_test!(
        name: seek_from_current_1,
        start_pos: Position{abs_pos: 4, rel_pos: 1, slice_idx: 1},
        seek: io::SeekFrom::Current(7),
        buf: b"foo",
        expect: Some(b"foobarfoobafoo"),
    );

    random_write_test!(
        name: seek_from_current_2,
        start_pos: Position{abs_pos: 4, rel_pos: 1, slice_idx: 1},
        seek: io::SeekFrom::Current(2),
        buf: b"foo",
        expect: Some(b"foobarfoobar"),
    );

    random_write_test!(
        name: seek_from_current_3,
        start_pos: Position{abs_pos: 4, rel_pos: 1, slice_idx: 1},
        seek: io::SeekFrom::Current(-2),
        buf: b"foo",
        expect: None,
    );
}
