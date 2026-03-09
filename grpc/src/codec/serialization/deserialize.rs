use super::Buf;

/// A trait for deserializing messages.
pub trait Deserialize {
    /// Decodes from a readable, abstract buffer, populating `self`.
    fn deserialize(&mut self, buf: &mut dyn Buf) -> Result<(), crate::Status>;
}
