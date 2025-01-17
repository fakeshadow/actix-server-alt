pub(crate) mod async_fn;
pub(crate) mod http;

use core::{future::Future, pin::Pin, time::Duration};

use crate::{body::BoxBody, client::Client, http::Request};
pub use http::HttpService;

type BoxFuture<'f, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'f>>;

/// trait for composable http services. Used for middleware,resolver and tls connector.
pub trait Service<Req> {
    type Response;
    type Error;

    fn call(&self, req: Req) -> impl Future<Output = Result<Self::Response, Self::Error>> + Send;
}

pub trait ServiceDyn<Req> {
    type Response;
    type Error;

    fn call<'s>(&'s self, req: Req) -> BoxFuture<'s, Self::Response, Self::Error>
    where
        Req: 's;
}

impl<S, Req> ServiceDyn<Req> for S
where
    S: Service<Req>,
{
    type Response = S::Response;
    type Error = S::Error;

    #[inline]
    fn call<'s>(&'s self, req: Req) -> BoxFuture<'s, Self::Response, Self::Error>
    where
        Req: 's,
    {
        Box::pin(Service::call(self, req))
    }
}

impl<I, Req> Service<Req> for Box<I>
where
    Req: Send,
    I: ServiceDyn<Req> + ?Sized + Send + Sync,
{
    type Response = I::Response;
    type Error = I::Error;

    #[inline]
    async fn call(&self, req: Req) -> Result<Self::Response, Self::Error> {
        ServiceDyn::call(&**self, req).await
    }
}

/// request type for middlewares.
/// It's similar to [RequestBuilder] type but with additional side effect enabled.
///
/// [RequestBuilder]: crate::request::RequestBuilder
pub struct ServiceRequest<'r, 'c> {
    pub req: &'r mut Request<BoxBody>,
    pub client: &'c Client,
    pub timeout: Duration,
}
