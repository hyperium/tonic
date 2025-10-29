use super::BufMut;

/// A trait for serializing messages.
pub trait Serialize {
    /// Encodes the message into a growable, abstract buffer.
    fn serialize(&self, buf: &mut dyn BufMut) -> Result<(), crate::Status>;
}
