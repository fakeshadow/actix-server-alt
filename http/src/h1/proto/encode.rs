use futures_core::stream::Stream;
use tracing::{debug, warn};

use crate::{
    body::BodySize,
    bytes::{Bytes, BytesMut},
    date::DateTime,
    http::{
        header::{HeaderMap, CONNECTION, CONTENT_LENGTH, DATE, TE, TRANSFER_ENCODING, UPGRADE},
        response::Parts,
        Extensions, StatusCode, Version,
    },
};

use super::{buf_write::H1BufWrite, codec::TransferCoding, context::Context, error::ProtoError, header};

pub const CONTINUE: &[u8; 25] = b"HTTP/1.1 100 Continue\r\n\r\n";

#[allow(clippy::declare_interior_mutable_const)]
pub const CONTINUE_BYTES: Bytes = Bytes::from_static(CONTINUE);

impl<D, const MAX_HEADERS: usize> Context<'_, D, MAX_HEADERS>
where
    D: DateTime,
{
    pub fn encode_head<B, W>(&mut self, parts: Parts, body: &B, buf: &mut W) -> Result<TransferCoding, ProtoError>
    where
        B: Stream,
        W: H1BufWrite,
    {
        buf.write_buf_head(|buf| self.encode_head_inner(parts, body, buf))
    }

    fn encode_head_inner<B>(&mut self, parts: Parts, body: &B, buf: &mut BytesMut) -> Result<TransferCoding, ProtoError>
    where
        B: Stream,
    {
        let version = parts.version;
        let status = parts.status;

        // decide if content-length or transfer-encoding header would be skipped.
        let skip_len = match status {
            StatusCode::SWITCHING_PROTOCOLS => true,
            // Sending content-length or transfer-encoding header on 2xx response
            // to CONNECT is forbidden in RFC 7231.
            s if self.is_connect_method() && s.is_success() => true,
            s if s.is_informational() => {
                warn!(target: "h1_encode", "response with 1xx status code not supported");
                return Err(ProtoError::Status);
            }
            _ => false,
        };

        // In some error cases, we don't know about the invalid message until already
        // pushing some bytes onto the `buf`. In those cases, we don't want to send
        // the half-pushed message, so rewind to before.
        // let orig_len = buf.len();

        // encode version, status code and reason
        encode_version_status_reason(buf, version, status);

        self.encode_headers(parts.headers, parts.extensions, body, buf, skip_len)
    }
}

#[inline]
fn encode_version_status_reason(buf: &mut BytesMut, version: Version, status: StatusCode) {
    // encode version, status code and reason
    match (version, status) {
        // happy path shortcut.
        (Version::HTTP_11, StatusCode::OK) => {
            buf.extend_from_slice(b"HTTP/1.1 200 OK");
            return;
        }
        (Version::HTTP_11, _) => {
            buf.extend_from_slice(b"HTTP/1.1 ");
        }
        (Version::HTTP_10, _) => {
            buf.extend_from_slice(b"HTTP/1.0 ");
        }
        _ => {
            debug!(target: "h1_encode", "response with unexpected response version");
            buf.extend_from_slice(b"HTTP/1.1 ");
        }
    }

    // a reason MUST be written, as many parsers will expect it.
    let reason = status.canonical_reason().unwrap_or("<none>").as_bytes();
    let status = status.as_str().as_bytes();
    buf.reserve(status.len() + reason.len() + 1);
    buf.extend_from_slice(status);
    buf.extend_from_slice(b" ");
    buf.extend_from_slice(reason);
}

impl<D, const MAX_HEADERS: usize> Context<'_, D, MAX_HEADERS>
where
    D: DateTime,
{
    pub fn encode_headers<B>(
        &mut self,
        mut headers: HeaderMap,
        mut extensions: Extensions,
        body: &B,
        buf: &mut BytesMut,
        mut skip_len: bool,
    ) -> Result<TransferCoding, ProtoError>
    where
        B: Stream,
    {
        let mut size = BodySize::from_stream(body);

        // strip body if response carry a body when responding to HEAD request.
        if self.is_head_method() {
            try_remove_body(buf, &headers, &mut size);
        }

        let mut skip_date = false;

        // use the shortest header name as default
        let mut name = TE;

        let mut encoding = TransferCoding::eof();

        for (next_name, value) in headers.drain() {
            let is_continue = match next_name {
                Some(next_name) => {
                    name = next_name;
                    false
                }
                None => true,
            };

            match name {
                CONNECTION => {
                    if self.is_connection_closed() {
                        // skip write header on close condition.
                        // the header is checked again and written properly afterwards.
                        continue;
                    }
                    self.try_set_close_from_header(&value)?;
                }
                UPGRADE => encoding = TransferCoding::upgrade(),
                DATE => skip_date = true,
                CONTENT_LENGTH => {
                    debug_assert!(!skip_len, "CONTENT_LENGTH header can not be set");
                    let value = header::parse_content_length(&value)?;
                    encoding = TransferCoding::length(value);
                    skip_len = true;
                }
                TRANSFER_ENCODING => {
                    debug_assert!(!skip_len, "TRANSFER_ENCODING header can not be set");
                    for val in value.to_str().map_err(|_| ProtoError::HeaderValue)?.split(',') {
                        let val = val.trim();
                        if val.eq_ignore_ascii_case("chunked") {
                            encoding = TransferCoding::encode_chunked();
                            skip_len = true;
                        }
                    }
                }
                _ => {}
            }

            let value = value.as_bytes();

            if is_continue {
                buf.reserve(value.len() + 2);
                buf.extend_from_slice(b", ");
                buf.extend_from_slice(value);
            } else {
                let name = name.as_str().as_bytes();
                buf.reserve(name.len() + value.len() + 4);
                buf.extend_from_slice(b"\r\n");
                buf.extend_from_slice(name);
                buf.extend_from_slice(b": ");
                buf.extend_from_slice(value);
            }
        }

        // encode transfer-encoding or content-length
        if !skip_len {
            match size {
                BodySize::None => {
                    encoding = TransferCoding::eof();
                }
                BodySize::Stream => {
                    buf.extend_from_slice(CHUNKED_HEADER);
                    encoding = TransferCoding::encode_chunked();
                }
                BodySize::Sized(size) => {
                    write_length_header(buf, size);
                    encoding = TransferCoding::length(size as u64);
                }
            }
        }

        if self.is_connection_closed() {
            buf.extend_from_slice(CLOSE_HEADER);
        }

        // set date header if there is not any.
        if !skip_date {
            buf.reserve(D::DATE_VALUE_LENGTH + 12);
            buf.extend_from_slice(b"\r\ndate: ");
            self.date().with_date(|slice| buf.extend_from_slice(slice));
        }

        buf.extend_from_slice(b"\r\n\r\n");

        // put header map back to cache.
        self.replace_headers(headers);

        // put extension back to cache;
        extensions.clear();
        self.replace_extensions(extensions);

        Ok(encoding)
    }
}

const CHUNKED_HEADER: &[u8; 28] = b"\r\ntransfer-encoding: chunked";
const CLOSE_HEADER: &[u8; 19] = b"\r\nconnection: close";

#[cold]
#[inline(never)]
fn try_remove_body(buf: &mut BytesMut, headers: &HeaderMap, size: &mut BodySize) {
    match *size {
        BodySize::None => return,
        BodySize::Stream => {
            if headers.contains_key(TRANSFER_ENCODING) {
                return;
            }
            buf.extend_from_slice(CHUNKED_HEADER);
        }
        BodySize::Sized(size) => {
            if headers.contains_key(CONTENT_LENGTH) {
                return;
            }
            write_length_header(buf, size);
        }
    }

    *size = BodySize::None;

    warn!("Response to HEAD request should not bearing body. It will been dropped without polling.");
}

fn write_length_header(buf: &mut BytesMut, size: usize) {
    let mut buffer = itoa::Buffer::new();
    let buffer = buffer.format(size).as_bytes();

    buf.reserve(buffer.len() + 18);
    buf.extend_from_slice(b"\r\ncontent-length: ");
    buf.extend_from_slice(buffer);
}

#[cfg(test)]
mod test {
    use crate::{
        body::{BoxBody, Once},
        bytes::Bytes,
        date::DateTimeService,
        http::{HeaderValue, Response},
    };

    use super::*;

    #[tokio::test]
    async fn append_header() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let date = DateTimeService::new();
                let mut ctx = Context::<_, 64>::new(date.get());

                let mut res = Response::new(BoxBody::new(Once::new(Bytes::new())));

                res.headers_mut()
                    .insert(CONNECTION, HeaderValue::from_static("keep-alive"));
                res.headers_mut()
                    .append(CONNECTION, HeaderValue::from_static("upgrade"));

                let (parts, body) = res.into_parts();

                let mut buf = BytesMut::new();
                ctx.encode_head(parts, &body, &mut buf).unwrap();

                let mut header = [httparse::EMPTY_HEADER; 8];
                let mut res = httparse::Response::new(&mut header);

                let httparse::Status::Complete(_) = res.parse(buf.as_ref()).unwrap() else {
                    panic!("failed to parse response")
                };

                for h in header {
                    if h.name == "connection" {
                        assert_eq!(h.value, b"keep-alive, upgrade");
                    }
                }
            })
            .await
    }
}
