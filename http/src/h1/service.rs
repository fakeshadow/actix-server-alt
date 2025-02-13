use core::{net::SocketAddr, pin::pin};

use futures_core::stream::Stream;
use xitca_io::io::AsyncIo;
use xitca_service::Service;

use crate::{
    bytes::Bytes,
    error::{HttpServiceError, TimeoutError},
    http::{Request, RequestExt, Response},
    service::HttpService,
    tls::IsTls,
    util::timer::Timeout,
};

use super::body::RequestBody;

pub type H1Service<St, S, A, const HEADER_LIMIT: usize, const READ_BUF_LIMIT: usize, const WRITE_BUF_LIMIT: usize> =
    HttpService<St, S, RequestBody, A, HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>;

impl<St, S, B, BE, A, const HEADER_LIMIT: usize, const READ_BUF_LIMIT: usize, const WRITE_BUF_LIMIT: usize>
    Service<(St, SocketAddr)> for H1Service<St, S, A, HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>
where
    S: Service<Request<RequestExt<RequestBody>>, Response = Response<B>>,
    A: Service<St> + IsTls,
    St: AsyncIo,
    A::Response: AsyncIo,
    B: Stream<Item = Result<Bytes, BE>>,
    HttpServiceError<S::Error, BE>: From<A::Error>,
{
    type Response = ();
    type Error = HttpServiceError<S::Error, BE>;

    async fn call(&self, (io, addr): (St, SocketAddr)) -> Result<Self::Response, Self::Error> {
        // at this stage keep-alive timer is used to tracks tls accept timeout.
        let mut timer = pin!(self.keep_alive());

        let mut io = self
            .tls_acceptor
            .call(io)
            .timeout(timer.as_mut())
            .await
            .map_err(|_| HttpServiceError::Timeout(TimeoutError::TlsAccept))??;

        super::dispatcher::run(
            &mut io,
            addr,
            timer,
            self.config,
            &self.service,
            self.date.get(),
            self.tls_acceptor.is_tls(),
        )
        .await
        .map_err(Into::into)
    }
}

#[cfg(feature = "io-uring")]
use {
    xitca_io::{
        io_uring::{AsyncBufRead, AsyncBufWrite},
        net::io_uring::TcpStream,
    },
    xitca_service::ready::ReadyService,
};

#[cfg(feature = "io-uring")]
use crate::{
    config::HttpServiceConfig,
    date::{DateTime, DateTimeService},
    util::timer::KeepAlive,
};

#[cfg(feature = "io-uring")]
pub struct H1UringService<S, A, const HEADER_LIMIT: usize, const READ_BUF_LIMIT: usize, const WRITE_BUF_LIMIT: usize> {
    pub(crate) config: HttpServiceConfig<HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>,
    pub(crate) date: DateTimeService,
    pub(crate) service: S,
    pub(crate) tls_acceptor: A,
}

#[cfg(feature = "io-uring")]
impl<S, A, const HEADER_LIMIT: usize, const READ_BUF_LIMIT: usize, const WRITE_BUF_LIMIT: usize>
    H1UringService<S, A, HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>
{
    pub(super) fn new(
        config: HttpServiceConfig<HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>,
        service: S,
        tls_acceptor: A,
    ) -> Self {
        Self {
            config,
            date: DateTimeService::new(),
            service,
            tls_acceptor,
        }
    }
}

#[cfg(feature = "io-uring")]
impl<S, B, BE, A, const HEADER_LIMIT: usize, const READ_BUF_LIMIT: usize, const WRITE_BUF_LIMIT: usize>
    Service<(TcpStream, SocketAddr)> for H1UringService<S, A, HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>
where
    S: Service<Request<RequestExt<RequestBody>>, Response = Response<B>>,
    A: Service<TcpStream> + IsTls,
    A::Response: AsyncBufRead + AsyncBufWrite + 'static,
    B: Stream<Item = Result<Bytes, BE>>,
    HttpServiceError<S::Error, BE>: From<A::Error>,
{
    type Response = ();
    type Error = HttpServiceError<S::Error, BE>;
    async fn call(&self, (io, addr): (TcpStream, SocketAddr)) -> Result<Self::Response, Self::Error> {
        let accept_dur = self.config.tls_accept_timeout;
        let deadline = self.date.get().now() + accept_dur;
        let mut timer = pin!(KeepAlive::new(deadline));

        let io = self
            .tls_acceptor
            .call(io)
            .timeout(timer.as_mut())
            .await
            .map_err(|_| HttpServiceError::Timeout(TimeoutError::TlsAccept))??;

        super::dispatcher_uring::Dispatcher::new(
            io,
            addr,
            timer,
            self.config,
            &self.service,
            self.date.get(),
            self.tls_acceptor.is_tls(),
        )
        .run()
        .await
        .map_err(Into::into)
    }
}

#[cfg(feature = "io-uring")]
impl<S, A, const HEADER_LIMIT: usize, const READ_BUF_LIMIT: usize, const WRITE_BUF_LIMIT: usize> ReadyService
    for H1UringService<S, A, HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>
where
    S: ReadyService,
{
    type Ready = S::Ready;

    #[inline]
    async fn ready(&self) -> Self::Ready {
        self.service.ready().await
    }
}
