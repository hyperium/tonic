use crate::server::call::metadata_writer::{InitialMetadataWriter, TrailingMetadataWriter};
use crate::server::call::{Metadata, StreamingResponseWriter};
use crate::server::stream::{PushStreamConsumer, PushStreamWriter};
use crate::Status;

/// A wrapper around PushStreamWriter that also supports sending metadata.
pub struct StreamingResponseImpl<C, I, Tr> {
    writer: PushStreamWriter<C>,
    initial_metadata_writer: I,
    trailing_metadata_writer: Tr,
}

impl<C, I, Tr> StreamingResponseImpl<C, I, Tr> {
    /// Creates a new StreamingResponseImpl.
    pub fn new(
        writer: PushStreamWriter<C>,
        initial_metadata_writer: I,
        trailing_metadata_writer: Tr,
    ) -> Self {
        Self {
            writer,
            initial_metadata_writer,
            trailing_metadata_writer,
        }
    }
}

impl<T, C, I, Tr> StreamingResponseWriter<T> for StreamingResponseImpl<C, I, Tr>
where
    T: Send,
    C: PushStreamConsumer<T> + Send + 'static,
    I: InitialMetadataWriter + Send,
    Tr: TrailingMetadataWriter + Send + 'static,
{
    type MessageWriter = C;
    type TrailerWriter = Tr;

    async fn send_initial_metadata(
        self,
        metadata: Metadata,
    ) -> Result<(Self::MessageWriter, Self::TrailerWriter), Status> {
        self.initial_metadata_writer
            .send_initial_metadata(metadata)
            .await?;
        Ok((self.writer.into_inner(), self.trailing_metadata_writer))
    }
}
