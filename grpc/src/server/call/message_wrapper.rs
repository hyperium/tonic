/// A raw message containing bytes and optional read options.
pub struct Incoming<B> {
    pub message_bytes: B,
    pub options: Option<MessageReadOptions>,
}

#[derive(Debug, Clone, Copy)]
pub struct MessageReadOptions {
    pub compressed: bool,
}

/// A wrapped message with optional write options.
pub struct Outgoing<T> {
    pub message: T,
    pub options: Option<MessageWriteOptions>,
}

impl<T> Outgoing<T> {
    pub fn new(message: T) -> Self {
        Self {
            message,
            options: None,
        }
    }

    pub fn with_options(message: T, options: MessageWriteOptions) -> Self {
        Self {
            message,
            options: Some(options),
        }
    }
}

impl<T> From<T> for Outgoing<T> {
    fn from(message: T) -> Self {
        Self::new(message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressionEncoding {
    #[default]
    Inherit,
    Enabled,
    Disabled,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MessageWriteOptions {
    pub compression: CompressionEncoding,
}
