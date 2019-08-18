use crate::{body::BytesBuf, Status};
use bytes::{BufMut, BytesMut, IntoBuf};
use futures_core::{Stream, TryStream};
use futures_util::StreamExt;
use tokio_codec::Encoder;

pub fn encode<T, U>(mut encoder: T, mut source: U) -> impl TryStream<Ok = BytesBuf, Error = Status>
where
    T: Encoder<Error = Status>,
    U: Stream<Item = Result<T::Item, Status>> + Unpin,
{
    async_stream::stream! {
        let mut buf = BytesMut::with_capacity(1024);

        loop {
            match source.next().await {
                Some(Ok(item)) => {
                    buf.reserve(5);
                    unsafe {
                        buf.advance_mut(5);
                    }
                    encoder.encode(item, &mut buf).map_err(drop).unwrap();

                    // now that we know length, we can write the header
                    let len = buf.len() - 5;
                    assert!(len <= std::u32::MAX as usize);
                    {
                        let mut cursor = std::io::Cursor::new(&mut buf[..5]);
                        cursor.put_u8(0); // byte must be 0, reserve doesn't auto-zero
                        cursor.put_u32_be(len as u32);
                    }

                    yield Ok(buf.split_to(len + 5).freeze().into_buf());
                },
                Some(Err(status)) => yield Err(status),
                None => break,
            }
        }
    }
}
