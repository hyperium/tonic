use std::error::Error;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use base64::Engine as _;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use http::{header, HeaderMap, HeaderName, HeaderValue};
use http_body::{Body, SizeHint};
use pin_project::pin_project;
use tokio_stream::Stream;
use tonic::Status;

use self::content_types::*;

// A grpc header is u8 (flag) + u32 (msg len)
const GRPC_HEADER_SIZE: usize = 1 + 4;

pub(crate) mod content_types {
    use http::{header::CONTENT_TYPE, HeaderMap};

    pub(crate) const GRPC_WEB: &str = "application/grpc-web";
    pub(crate) const GRPC_WEB_PROTO: &str = "application/grpc-web+proto";
    pub(crate) const GRPC_WEB_TEXT: &str = "application/grpc-web-text";
    pub(crate) const GRPC_WEB_TEXT_PROTO: &str = "application/grpc-web-text+proto";

    pub(crate) fn is_grpc_web(headers: &HeaderMap) -> bool {
        matches!(
            content_type(headers),
            Some(GRPC_WEB) | Some(GRPC_WEB_PROTO) | Some(GRPC_WEB_TEXT) | Some(GRPC_WEB_TEXT_PROTO)
        )
    }

    fn content_type(headers: &HeaderMap) -> Option<&str> {
        headers.get(CONTENT_TYPE).and_then(|val| val.to_str().ok())
    }
}

const BUFFER_SIZE: usize = 8 * 1024;

const FRAME_HEADER_SIZE: usize = 5;

// 8th (MSB) bit of the 1st gRPC frame byte
// denotes an uncompressed trailer (as part of the body)
const GRPC_WEB_TRAILERS_BIT: u8 = 0b10000000;

#[derive(Copy, Clone, PartialEq, Debug)]
enum Direction {
    Decode,
    Encode,
    Empty,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub(crate) enum Encoding {
    Base64,
    None,
}

/// HttpBody adapter for the grpc web based services.
#[derive(Debug)]
#[pin_project]
pub struct GrpcWebCall<B> {
    #[pin]
    inner: B,
    buf: BytesMut,
    direction: Direction,
    encoding: Encoding,
    poll_trailers: bool,
    client: bool,
    trailers: Option<HeaderMap>,
}

impl<B: Default> Default for GrpcWebCall<B> {
    fn default() -> Self {
        Self {
            inner: Default::default(),
            buf: Default::default(),
            direction: Direction::Empty,
            encoding: Encoding::None,
            poll_trailers: Default::default(),
            client: Default::default(),
            trailers: Default::default(),
        }
    }
}

impl<B> GrpcWebCall<B> {
    pub(crate) fn request(inner: B, encoding: Encoding) -> Self {
        Self::new(inner, Direction::Decode, encoding)
    }

    pub(crate) fn response(inner: B, encoding: Encoding) -> Self {
        Self::new(inner, Direction::Encode, encoding)
    }

    pub(crate) fn client_request(inner: B) -> Self {
        Self::new_client(inner, Direction::Encode, Encoding::None)
    }

    pub(crate) fn client_response(inner: B) -> Self {
        Self::new_client(inner, Direction::Decode, Encoding::None)
    }

    fn new_client(inner: B, direction: Direction, encoding: Encoding) -> Self {
        GrpcWebCall {
            inner,
            buf: BytesMut::with_capacity(match (direction, encoding) {
                (Direction::Encode, Encoding::Base64) => BUFFER_SIZE,
                _ => 0,
            }),
            direction,
            encoding,
            poll_trailers: true,
            client: true,
            trailers: None,
        }
    }

    fn new(inner: B, direction: Direction, encoding: Encoding) -> Self {
        GrpcWebCall {
            inner,
            buf: BytesMut::with_capacity(match (direction, encoding) {
                (Direction::Encode, Encoding::Base64) => BUFFER_SIZE,
                _ => 0,
            }),
            direction,
            encoding,
            poll_trailers: true,
            client: false,
            trailers: None,
        }
    }

    // This is to avoid passing a slice of bytes with a length that the base64
    // decoder would consider invalid.
    #[inline]
    fn max_decodable(&self) -> usize {
        (self.buf.len() / 4) * 4
    }

    fn decode_chunk(mut self: Pin<&mut Self>) -> Result<Option<Bytes>, Status> {
        // not enough bytes to decode
        if self.buf.is_empty() || self.buf.len() < 4 {
            return Ok(None);
        }

        // Split `buf` at the largest index that is multiple of 4. Decode the
        // returned `Bytes`, keeping the rest for the next attempt to decode.
        let index = self.max_decodable();

        crate::util::base64::STANDARD
            .decode(self.as_mut().project().buf.split_to(index))
            .map(|decoded| Some(Bytes::from(decoded)))
            .map_err(internal_error)
    }
}

impl<B> GrpcWebCall<B>
where
    B: Body<Data = Bytes>,
    B::Error: Error,
{
    fn poll_decode(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<B::Data, Status>>> {
        match self.encoding {
            Encoding::Base64 => loop {
                if let Some(bytes) = self.as_mut().decode_chunk()? {
                    return Poll::Ready(Some(Ok(bytes)));
                }

                let mut this = self.as_mut().project();

                match ready!(this.inner.as_mut().poll_data(cx)) {
                    Some(Ok(data)) => this.buf.put(data),
                    Some(Err(e)) => return Poll::Ready(Some(Err(internal_error(e)))),
                    None => {
                        return if this.buf.has_remaining() {
                            Poll::Ready(Some(Err(internal_error("malformed base64 request"))))
                        } else {
                            Poll::Ready(None)
                        }
                    }
                }
            },

            Encoding::None => match ready!(self.project().inner.poll_data(cx)) {
                Some(res) => Poll::Ready(Some(res.map_err(internal_error))),
                None => Poll::Ready(None),
            },
        }
    }

    fn poll_encode(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<B::Data, Status>>> {
        let mut this = self.as_mut().project();

        if let Some(mut res) = ready!(this.inner.as_mut().poll_data(cx)) {
            if *this.encoding == Encoding::Base64 {
                res = res.map(|b| crate::util::base64::STANDARD.encode(b).into())
            }

            return Poll::Ready(Some(res.map_err(internal_error)));
        }

        // this flag is needed because the inner stream never
        // returns Poll::Ready(None) when polled for trailers
        if *this.poll_trailers {
            return match ready!(this.inner.poll_trailers(cx)) {
                Ok(Some(map)) => {
                    let mut frame = make_trailers_frame(map);

                    if *this.encoding == Encoding::Base64 {
                        frame = crate::util::base64::STANDARD.encode(frame).into_bytes();
                    }

                    *this.poll_trailers = false;
                    Poll::Ready(Some(Ok(frame.into())))
                }
                Ok(None) => Poll::Ready(None),
                Err(e) => Poll::Ready(Some(Err(internal_error(e)))),
            };
        }

        Poll::Ready(None)
    }
}

impl<B> Body for GrpcWebCall<B>
where
    B: Body<Data = Bytes>,
    B::Error: Error,
{
    type Data = Bytes;
    type Error = Status;

    fn poll_data(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        if self.client && self.direction == Direction::Decode {
            let mut me = self.as_mut();

            loop {
                let incoming_buf = match ready!(me.as_mut().poll_decode(cx)) {
                    Some(Ok(incoming_buf)) => incoming_buf,
                    None => {
                        // TODO: Consider eofing here?
                        // Even if the buffer has more data, this will hit the eof branch
                        // of decode in tonic
                        return Poll::Ready(None);
                    }
                    Some(Err(e)) => return Poll::Ready(Some(Err(e))),
                };

                let buf = &mut me.as_mut().project().buf;

                buf.put(incoming_buf);

                return match find_trailers(&buf[..])? {
                    FindTrailers::Trailer(len) => {
                        // Extract up to len of where the trailers are at
                        let msg_buf = buf.copy_to_bytes(len);
                        match decode_trailers_frame(buf.split().freeze()) {
                            Ok(Some(trailers)) => {
                                self.project().trailers.replace(trailers);
                            }
                            Err(e) => return Poll::Ready(Some(Err(e))),
                            _ => {}
                        }

                        if msg_buf.has_remaining() {
                            Poll::Ready(Some(Ok(msg_buf)))
                        } else {
                            Poll::Ready(None)
                        }
                    }
                    FindTrailers::IncompleteBuf => continue,
                    FindTrailers::Done(len) => Poll::Ready(Some(Ok(buf.split_to(len).freeze()))),
                };
            }
        }

        match self.direction {
            Direction::Decode => self.poll_decode(cx),
            Direction::Encode => self.poll_encode(cx),
            Direction::Empty => Poll::Ready(None),
        }
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Result<Option<HeaderMap<HeaderValue>>, Self::Error>> {
        let trailers = self.project().trailers.take();
        Poll::Ready(Ok(trailers))
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl<B> Stream for GrpcWebCall<B>
where
    B: Body<Data = Bytes>,
    B::Error: Error,
{
    type Item = Result<Bytes, Status>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Body::poll_data(self, cx)
    }
}

impl Encoding {
    pub(crate) fn from_content_type(headers: &HeaderMap) -> Encoding {
        Self::from_header(headers.get(header::CONTENT_TYPE))
    }

    pub(crate) fn from_accept(headers: &HeaderMap) -> Encoding {
        Self::from_header(headers.get(header::ACCEPT))
    }

    pub(crate) fn to_content_type(self) -> &'static str {
        match self {
            Encoding::Base64 => GRPC_WEB_TEXT_PROTO,
            Encoding::None => GRPC_WEB_PROTO,
        }
    }

    fn from_header(value: Option<&HeaderValue>) -> Encoding {
        match value.and_then(|val| val.to_str().ok()) {
            Some(GRPC_WEB_TEXT_PROTO) | Some(GRPC_WEB_TEXT) => Encoding::Base64,
            _ => Encoding::None,
        }
    }
}

fn internal_error(e: impl std::fmt::Display) -> Status {
    Status::internal(format!("tonic-web: {}", e))
}

// Key-value pairs encoded as a HTTP/1 headers block (without the terminating newline)
fn encode_trailers(trailers: HeaderMap) -> Vec<u8> {
    trailers.iter().fold(Vec::new(), |mut acc, (key, value)| {
        acc.put_slice(key.as_ref());
        acc.push(b':');
        acc.put_slice(value.as_bytes());
        acc.put_slice(b"\r\n");
        acc
    })
}

fn decode_trailers_frame(mut buf: Bytes) -> Result<Option<HeaderMap>, Status> {
    if buf.remaining() < GRPC_HEADER_SIZE {
        return Ok(None);
    }

    buf.get_u8();
    buf.get_u32();

    let mut map = HeaderMap::new();
    let mut temp_buf = buf.clone();

    let mut trailers = Vec::new();
    let mut cursor_pos = 0;

    for (i, b) in buf.iter().enumerate() {
        if b == &b'\r' && buf.get(i + 1) == Some(&b'\n') {
            let trailer = temp_buf.copy_to_bytes(i - cursor_pos);
            cursor_pos = i;
            trailers.push(trailer);
            if temp_buf.has_remaining() {
                temp_buf.get_u8();
                temp_buf.get_u8();
            }
        }
    }

    for trailer in trailers {
        let mut s = trailer.split(|b| b == &b':');
        let key = s
            .next()
            .ok_or_else(|| Status::internal("trailers couldn't parse key"))?;
        let value = s
            .next()
            .ok_or_else(|| Status::internal("trailers couldn't parse value"))?;

        let value = value
            .split(|b| b == &b'\r')
            .next()
            .ok_or_else(|| Status::internal("trailers was not escaped"))?;

        let header_key = HeaderName::try_from(key)
            .map_err(|e| Status::internal(format!("Unable to parse HeaderName: {}", e)))?;
        let header_value = HeaderValue::try_from(value)
            .map_err(|e| Status::internal(format!("Unable to parse HeaderValue: {}", e)))?;
        map.insert(header_key, header_value);
    }

    Ok(Some(map))
}

fn make_trailers_frame(trailers: HeaderMap) -> Vec<u8> {
    let trailers = encode_trailers(trailers);
    let len = trailers.len();
    assert!(len <= u32::MAX as usize);

    let mut frame = Vec::with_capacity(len + FRAME_HEADER_SIZE);
    frame.push(GRPC_WEB_TRAILERS_BIT);
    frame.put_u32(len as u32);
    frame.extend(trailers);

    frame
}

/// Search some buffer for grpc-web trailers headers and return
/// its location in the original buf. If `None` is returned we did
/// not find a trailers in this buffer either because its incomplete
/// or the buffer just contained grpc message frames.
fn find_trailers(buf: &[u8]) -> Result<FindTrailers, Status> {
    let mut len = 0;
    let mut temp_buf = buf;

    loop {
        // To check each frame, there must be at least GRPC_HEADER_SIZE
        // amount of bytes available otherwise the buffer is incomplete.
        if temp_buf.is_empty() || temp_buf.len() < GRPC_HEADER_SIZE {
            return Ok(FindTrailers::Done(len));
        }

        let header = temp_buf.get_u8();

        if header == GRPC_WEB_TRAILERS_BIT {
            return Ok(FindTrailers::Trailer(len));
        }

        if !(header == 0 || header == 1) {
            return Err(Status::internal(format!(
                "Invalid header bit {} expected 0 or 1",
                header
            )));
        }

        let msg_len = temp_buf.get_u32();

        len += msg_len as usize + 4 + 1;

        // If the msg len of a non-grpc-web trailer frame is larger than
        // the overall buffer we know within that buffer there are no trailers.
        if len > buf.len() {
            return Ok(FindTrailers::IncompleteBuf);
        }

        temp_buf = &buf[len..];
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FindTrailers {
    Trailer(usize),
    IncompleteBuf,
    Done(usize),
}

#[cfg(test)]
mod tests {
    use tonic::Code;

    use super::*;

    #[test]
    fn encoding_constructors() {
        let cases = &[
            (GRPC_WEB, Encoding::None),
            (GRPC_WEB_PROTO, Encoding::None),
            (GRPC_WEB_TEXT, Encoding::Base64),
            (GRPC_WEB_TEXT_PROTO, Encoding::Base64),
            ("foo", Encoding::None),
        ];

        let mut headers = HeaderMap::new();

        for case in cases {
            headers.insert(header::CONTENT_TYPE, case.0.parse().unwrap());
            headers.insert(header::ACCEPT, case.0.parse().unwrap());

            assert_eq!(Encoding::from_content_type(&headers), case.1, "{}", case.0);
            assert_eq!(Encoding::from_accept(&headers), case.1, "{}", case.0);
        }
    }

    #[test]
    fn decode_trailers() {
        let mut headers = HeaderMap::new();
        headers.insert("grpc-status", 0.try_into().unwrap());
        headers.insert("grpc-message", "this is a message".try_into().unwrap());

        let trailers = make_trailers_frame(headers.clone());

        let buf = Bytes::from(trailers);

        let map = decode_trailers_frame(buf).unwrap().unwrap();

        assert_eq!(headers, map);
    }

    #[test]
    fn find_trailers_non_buffered() {
        // Byte version of this:
        // b"\x80\0\0\0\x0fgrpc-status:0\r\n"
        let buf = [
            128, 0, 0, 0, 15, 103, 114, 112, 99, 45, 115, 116, 97, 116, 117, 115, 58, 48, 13, 10,
        ];

        let out = find_trailers(&buf[..]).unwrap();

        assert_eq!(out, FindTrailers::Trailer(0));
    }

    #[test]
    fn find_trailers_buffered() {
        // Byte version of this:
        // b"\0\0\0\0L\n$975738af-1a17-4aea-b887-ed0bbced6093\x1a$da609e9b-f470-4cc0-a691-3fd6a005a436\x80\0\0\0\x0fgrpc-status:0\r\n"
        let buf = [
            0, 0, 0, 0, 76, 10, 36, 57, 55, 53, 55, 51, 56, 97, 102, 45, 49, 97, 49, 55, 45, 52,
            97, 101, 97, 45, 98, 56, 56, 55, 45, 101, 100, 48, 98, 98, 99, 101, 100, 54, 48, 57,
            51, 26, 36, 100, 97, 54, 48, 57, 101, 57, 98, 45, 102, 52, 55, 48, 45, 52, 99, 99, 48,
            45, 97, 54, 57, 49, 45, 51, 102, 100, 54, 97, 48, 48, 53, 97, 52, 51, 54, 128, 0, 0, 0,
            15, 103, 114, 112, 99, 45, 115, 116, 97, 116, 117, 115, 58, 48, 13, 10,
        ];

        let out = find_trailers(&buf[..]).unwrap();

        assert_eq!(out, FindTrailers::Trailer(81));

        let trailers = decode_trailers_frame(Bytes::copy_from_slice(&buf[81..]))
            .unwrap()
            .unwrap();
        let status = trailers.get("grpc-status").unwrap();
        assert_eq!(status.to_str().unwrap(), "0")
    }

    #[test]
    fn find_trailers_buffered_incomplete_message() {
        let buf = vec![
            0, 0, 0, 9, 238, 10, 233, 19, 18, 230, 19, 10, 9, 10, 1, 120, 26, 4, 84, 69, 88, 84,
            18, 60, 10, 58, 10, 56, 3, 0, 0, 0, 44, 0, 0, 0, 0, 0, 0, 0, 116, 104, 105, 115, 32,
            118, 97, 108, 117, 101, 32, 119, 97, 115, 32, 119, 114, 105, 116, 116, 101, 110, 32,
            118, 105, 97, 32, 119, 114, 105, 116, 101, 32, 100, 101, 108, 101, 103, 97, 116, 105,
            111, 110, 33, 18, 62, 10, 60, 10, 58, 3, 0, 0, 0, 46, 0, 0, 0, 0, 0, 0, 0, 116, 104,
            105, 115, 32, 118, 97, 108, 117, 101, 32, 119, 97, 115, 32, 119, 114, 105, 116, 116,
            101, 110, 32, 98, 121, 32, 97, 110, 32, 101, 109, 98, 101, 100, 100, 101, 100, 32, 114,
            101, 112, 108, 105, 99, 97, 33, 18, 62, 10, 60, 10, 58, 3, 0, 0, 0, 46, 0, 0, 0, 0, 0,
            0, 0, 116, 104, 105, 115, 32, 118, 97, 108, 117, 101, 32, 119, 97, 115, 32, 119, 114,
            105, 116, 116, 101, 110, 32, 98, 121, 32, 97, 110, 32, 101, 109, 98, 101, 100, 100,
            101, 100, 32, 114, 101, 112, 108, 105, 99, 97, 33, 18, 62, 10, 60, 10, 58, 3, 0, 0, 0,
            46, 0, 0, 0, 0, 0, 0, 0, 116, 104, 105, 115, 32, 118, 97, 108, 117, 101, 32, 119, 97,
            115, 32, 119, 114, 105, 116, 116, 101, 110, 32, 98, 121, 32, 97, 110, 32, 101, 109, 98,
            101, 100, 100, 101, 100, 32, 114, 101, 112, 108, 105, 99, 97, 33, 18, 62, 10, 60, 10,
            58, 3, 0, 0, 0, 46, 0, 0, 0, 0, 0, 0, 0, 116, 104, 105, 115, 32, 118, 97, 108, 117,
            101, 32, 119, 97, 115, 32, 119, 114, 105, 116, 116, 101, 110, 32, 98, 121, 32, 97, 110,
            32, 101, 109, 98, 101, 100, 100, 101, 100, 32, 114, 101, 112, 108, 105, 99, 97, 33, 18,
            62, 10, 60, 10, 58, 3, 0, 0, 0, 46, 0, 0, 0, 0, 0, 0, 0, 116, 104, 105, 115, 32, 118,
            97, 108, 117, 101, 32, 119, 97, 115, 32, 119, 114, 105, 116, 116, 101, 110, 32, 98,
            121, 32, 97, 110, 32, 101, 109, 98, 101, 100, 100, 101, 100, 32, 114, 101, 112, 108,
            105, 99, 97, 33, 18, 62, 10, 60, 10, 58, 3, 0, 0, 0, 46, 0, 0, 0, 0, 0, 0, 0, 116, 104,
            105, 115, 32, 118, 97, 108, 117, 101, 32, 119, 97, 115, 32, 119, 114, 105, 116, 116,
            101, 110, 32, 98, 121, 32, 97, 110, 32, 101, 109, 98, 101, 100, 100, 101, 100, 32, 114,
            101, 112, 108, 105, 99, 97, 33, 18, 62, 10, 60, 10, 58, 3, 0, 0, 0, 46, 0, 0, 0, 0, 0,
            0, 0, 116, 104, 105, 115, 32, 118, 97, 108, 117, 101, 32, 119, 97, 115, 32, 119, 114,
            105, 116, 116, 101, 110, 32, 98, 121, 32,
        ];

        let out = find_trailers(&buf[..]).unwrap();

        assert_eq!(out, FindTrailers::IncompleteBuf);
    }

    #[test]
    #[ignore]
    fn find_trailers_buffered_incomplete_buf_bug() {
        let buf = std::fs::read("tests/incomplete-buf-bug.bin").unwrap();
        let out = find_trailers(&buf[..]).unwrap_err();

        assert_eq!(out.code(), Code::Internal);
    }
}
