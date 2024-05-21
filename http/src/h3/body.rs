use core::{
    pin::Pin,
    task::{Context, Poll},
};

use ::h3::server::RequestStream;
use futures_core::stream::Stream;
use h3_quinn::RecvStream;

use crate::{
    bytes::{Buf, Bytes},
    error::BodyError,
};

/// Request body type for Http/3 specifically.
pub struct RequestBody(pub(super) RequestStream<RecvStream, Bytes>);

impl Stream for RequestBody {
    type Item = Result<Bytes, BodyError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut()
            .0
            .poll_recv_data(cx)?
            .map(|res| res.map(|buf| Ok(Bytes::copy_from_slice(buf.chunk()))))
    }
}

impl From<RequestBody> for crate::body::RequestBody {
    fn from(body: RequestBody) -> Self {
        Self::H3(body)
    }
}
