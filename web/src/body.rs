//! http body types and traits.

use futures_core::stream::Stream;

pub use xitca_http::body::{none_body_hint, BoxBody, RequestBody, ResponseBody, NONE_BODY_HINT};

pub(crate) use xitca_http::body::Either;

use crate::error::BodyError;

/// an extended trait for [Stream] that specify additional type info of the [Stream::Item] type.
pub trait BodyStream: Stream<Item = Result<Self::Chunk, Self::Error>> {
    type Chunk: AsRef<[u8]> + 'static;
    type Error: Into<BodyError>;
}

impl<S, T, E> BodyStream for S
where
    S: Stream<Item = Result<T, E>>,
    T: AsRef<[u8]> + 'static,
    E: Into<BodyError>,
{
    type Chunk = T;
    type Error = E;
}

#[cfg(feature = "nightly")]
pub use nightly::AsyncBody;

#[cfg(feature = "nightly")]
mod nightly {
    use core::{
        async_iter::AsyncIterator,
        pin::Pin,
        task::{Context, Poll},
    };

    use pin_project_lite::pin_project;

    use super::*;
    pin_project! {
        pub struct AsyncBody<B> {
            #[pin]
            inner: B
        }

    }

    impl<B> From<B> for AsyncBody<B>
    where
        B: AsyncIterator,
    {
        fn from(inner: B) -> Self {
            Self { inner }
        }
    }

    impl<B> Stream for AsyncBody<B>
    where
        B: AsyncIterator,
    {
        type Item = <B as AsyncIterator>::Item;

        #[inline]
        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            AsyncIterator::poll_next(self.project().inner, cx)
        }
    }
}
