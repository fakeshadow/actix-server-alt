use std::{
    fmt,
    future::Future,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures_core::{ready, Stream};
use http::{Request, Response};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    pin,
};
use xitca_service::Service;

use crate::body::ResponseBody;
use crate::error::{BodyError, HttpServiceError, TimeoutError};
use crate::service::HttpService;
use crate::util::futures::Timeout;

use super::body::RequestBody;
use super::proto::Dispatcher;

pub type H2Service<S, A, const HEADER_LIMIT: usize, const READ_BUF_LIMIT: usize, const WRITE_BUF_LIMIT: usize> =
    HttpService<S, RequestBody, (), (), A, HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>;

impl<St, S, B, E, A, TlsSt, const HEADER_LIMIT: usize, const READ_BUF_LIMIT: usize, const WRITE_BUF_LIMIT: usize>
    Service<St> for H2Service<S, A, HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>
where
    S: Service<Request<RequestBody>, Response = Response<ResponseBody<B>>> + 'static,
    S::Error: fmt::Debug,

    A: Service<St, Response = TlsSt> + 'static,

    B: Stream<Item = Result<Bytes, E>> + 'static,
    E: 'static,
    BodyError: From<E>,

    St: AsyncRead + AsyncWrite + Unpin,
    TlsSt: AsyncRead + AsyncWrite + Unpin,

    HttpServiceError<S::Error>: From<A::Error>,
{
    type Response = ();
    type Error = HttpServiceError<S::Error>;
    type Future<'f> = impl Future<Output = Result<Self::Response, Self::Error>>;

    #[inline]
    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self
            .tls_acceptor
            .poll_ready(cx)
            .map_err(|_| HttpServiceError::ServiceReady))?;

        self.flow
            .service
            .poll_ready(cx)
            .map_err(|_| HttpServiceError::ServiceReady)
    }

    fn call(&self, io: St) -> Self::Future<'_> {
        async move {
            // tls accept timer.
            let timer = self.keep_alive();
            pin!(timer);

            let tls_stream = self
                .tls_acceptor
                .call(io)
                .timeout(timer.as_mut())
                .await
                .map_err(|_| HttpServiceError::Timeout(TimeoutError::TlsAccept))??;

            // update timer to first request timeout.
            self.update_first_request_deadline(timer.as_mut());

            let mut conn = ::h2::server::handshake(tls_stream)
                .timeout(timer.as_mut())
                .await
                .map_err(|_| HttpServiceError::Timeout(TimeoutError::H2Handshake))??;

            let dispatcher = Dispatcher::new(
                &mut conn,
                timer.as_mut(),
                self.config.keep_alive_timeout,
                &self.flow,
                self.date.get_shared(),
            );

            dispatcher.run().await?;

            Ok(())
        }
    }
}
