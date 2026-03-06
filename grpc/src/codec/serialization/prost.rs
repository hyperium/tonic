use super::{Buf, BufMut, Deserialize, Serialize};

impl<T> Serialize for T
where
    T: prost::Message,
{
    fn serialize(&self, buf: &mut dyn BufMut) -> Result<(), crate::Status> {
        self.encode(buf)
            .map_err(|e| crate::Status::new(crate::StatusCode::Internal, e.to_string()))
    }
}

impl<T> Deserialize for T
where
    T: prost::Message + Default,
{
    fn deserialize(&mut self, buf: &mut dyn Buf) -> Result<(), crate::Status> {
        self.clear();
        self.merge(buf)
            .map_err(|e| crate::Status::new(crate::StatusCode::Internal, e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn test_prost_serialization() {
        use prost::Message;

        #[derive(Clone, PartialEq, Message)]
        struct TestMessage {
            #[prost(string, tag = "1")]
            pub name: String,
        }

        let msg = TestMessage {
            name: "test".to_string(),
        };

        let mut buf = BytesMut::new();
        Serialize::serialize(&msg, &mut buf).unwrap();

        assert!(!buf.is_empty());
        let decoded = TestMessage::decode(buf.as_ref()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_prost_deserialization() {
        use prost::Message;

        #[derive(Clone, PartialEq, Message)]
        struct TestMessage {
            #[prost(string, tag = "1")]
            pub name: String,
        }

        let msg = TestMessage {
            name: "test".to_string(),
        };
        let mut buf = BytesMut::new();
        msg.encode(&mut buf).unwrap();

        let mut deserialized = TestMessage::default();
        Deserialize::deserialize(&mut deserialized, &mut buf).unwrap();

        assert_eq!(deserialized, msg);
    }
}
