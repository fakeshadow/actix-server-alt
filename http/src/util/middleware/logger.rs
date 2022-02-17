use std::{fmt::Debug, future::Future};

use tracing::{error, span, Level, Span};
use xitca_service::{ready::ReadyService, Service, ServiceFactory};

/// A factory for logger service.
#[derive(Clone)]
pub struct Logger {
    span: Span,
}

impl Default for Logger {
    fn default() -> Self {
        Self::new()
    }
}

impl Logger {
    pub fn new() -> Self {
        Self::with_span(span!(Level::TRACE, "xitca-logger"))
    }

    pub fn with_span(span: Span) -> Self {
        Self { span }
    }
}

impl<S, Req> ServiceFactory<Req, S> for Logger
where
    S: Service<Req>,
    S::Error: Debug,
{
    type Response = S::Response;
    type Error = S::Error;
    type Service = LoggerService<S>;
    type Future = impl Future<Output = Result<Self::Service, Self::Error>>;

    fn new_service(&self, service: S) -> Self::Future {
        let span = self.span.clone();
        async move { Ok(LoggerService { service, span }) }
    }
}

/// Logger service uses a tracking span called `xitca_http_logger` and would collect
/// log from all levels(from trace to info)
pub struct LoggerService<S> {
    service: S,
    span: Span,
}

impl<S, Req> Service<Req> for LoggerService<S>
where
    S: Service<Req>,
    S::Error: Debug,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future<'f>
    where
        S: 'f,
    = impl Future<Output = Result<Self::Response, Self::Error>>;

    #[inline]
    fn call(&self, req: Req) -> Self::Future<'_> {
        async move {
            let _enter = self.span.enter();
            self.service.call(req).await.map_err(|e| {
                error!("{:?}", e);
                e
            })
        }
    }
}

impl<S, Req> ReadyService<Req> for LoggerService<S>
where
    S: ReadyService<Req>,
    S::Error: Debug,
{
    type Ready = S::Ready;

    type ReadyFuture<'f>
    where
        Self: 'f,
    = S::ReadyFuture<'f>;

    #[inline]
    fn ready(&self) -> Self::ReadyFuture<'_> {
        self.service.ready()
    }
}
