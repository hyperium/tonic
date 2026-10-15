/*
 *
 * Copyright 2026 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

pub(crate) mod compression;

use bytes::Buf;
use bytes::BufMut;
use bytes::Bytes;
use tonic::Status as TonicStatus;
use tonic::codec::DecodeBuf;
use tonic::codec::Decoder;
use tonic::codec::EncodeBuf;
use tonic::codec::Encoder;

#[derive(Debug, Default, Clone)]
pub struct BytesDecoder;

impl Decoder for BytesDecoder {
    type Item = Bytes;
    type Error = TonicStatus;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}

#[derive(Debug, Default, Clone)]
pub struct BufEncoder;

impl Encoder for BufEncoder {
    type Item = Box<dyn Buf + Send + Sync>;
    type Error = TonicStatus;

    fn encode(&mut self, mut item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        dst.put(&mut *item);
        Ok(())
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct BufCodec;

impl tonic::codec::Codec for BufCodec {
    type Encode = Box<dyn Buf + Send + Sync>;
    type Decode = Bytes;
    type Encoder = BufEncoder;
    type Decoder = BytesDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        BufEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        BytesDecoder
    }
}
