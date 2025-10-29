use bytes::Bytes;

use super::{Buf, BufMut, Deserialize, Serialize};

impl<T> Serialize for T
where
    T: protobuf::Serialize,
{
    fn serialize(&self, buf: &mut dyn BufMut) -> Result<(), crate::Status> {
        let payload = self
            .serialize()
            .map_err(|e| crate::Status::new(crate::StatusCode::Internal, e.to_string()))?;
        // Convert to Bytes first because the `put_slice` will always create a copy.
        // But insertion of `Bytes` can be optimized by the underlying Buf Implementation
        // to avoid copying.
        buf.put_buf(&mut Bytes::from(payload));
        Ok(())
    }
}

// Deserializes `buf` to `message`.
fn deserialize_proto_message<T: protobuf::ClearAndParse>(
    message: &mut T,
    buf: &'_ mut dyn Buf,
) -> Result<(), crate::Status> {
    // Since protobuf library expects a contiguous block of memory
    // Copy to a Bytes. The efficiency of this operation is underlying
    // `Buf` implementation dependent. It's O(1) for something like `Bytes` or
    // `vec[u8]` and may result in a copy for non contiguous `Buf`.
    let payload = buf.copy_to_bytes(buf.remaining());
    message
        .clear_and_parse(&payload)
        .map_err(|e| crate::Status::new(crate::StatusCode::Internal, e.to_string()))?;
    Ok(())
}

impl<T> Deserialize for T
where
    T: protobuf::ClearAndParse,
{
    fn deserialize(&mut self, buf: &mut dyn Buf) -> Result<(), crate::Status> {
        deserialize_proto_message(self, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn test_protobuf_serialization() {
        use protobuf_well_known_types::Timestamp;

        let mut request = Timestamp::new();
        request.set_seconds(1234567890);
        request.set_nanos(123);

        let mut buf = BytesMut::new();
        Serialize::serialize(&request, &mut buf).unwrap();
        assert_eq!(
            buf.to_vec(),
            protobuf::Serialize::serialize(&request).unwrap()
        );
    }

    #[test]
    fn test_protobuf_deserialization() {
        use protobuf::Serialize as ProtobufSerialize;
        use protobuf_well_known_types::Timestamp;

        let mut request = Timestamp::new();
        request.set_seconds(1234567890);
        request.set_nanos(123);
        let mut buf = Bytes::from(ProtobufSerialize::serialize(&request).unwrap());
        let mut deserialized = Timestamp::new();
        Deserialize::deserialize(&mut deserialized, &mut buf).unwrap();

        assert_eq!(deserialized.seconds(), 1234567890);
        assert_eq!(deserialized.nanos(), 123);
    }
}
