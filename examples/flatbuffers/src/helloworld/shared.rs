/// Helper function to finish a builder and return it as its root type with a `bytes::Bytes` buffer
pub fn build_into<R, T>(
    mut builder: butte::FlatBufferBuilder,
    root: butte::WIPOffset<R>,
) -> Result<T, butte::Error>
where
    T: From<butte::Table<bytes::Bytes>>,
{
    builder.finish_minimal(root);
    let (mut buf, header_size) = builder.collapse(); // (Vec<u8>, usize)
    let buf = buf.split_off(header_size); // we discard the builder header

    let bytes = bytes::Bytes::from(buf);
    let table = butte::get_root::<butte::Table<bytes::Bytes>, bytes::Bytes>(bytes)?;
    Ok(T::from(table))
}
