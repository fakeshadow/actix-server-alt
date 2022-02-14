use std::{fmt, future::Future};

use futures_core::Stream;
use xitca_io::net::UdpStream;
use xitca_service::ServiceFactory;

use crate::{body::ResponseBody, bytes::Bytes, error::HttpServiceError, http::Response, request::Request};

use super::{body::RequestBody, service::H3Service};

/// Http/3 Builder type.
/// Take in generic types of ServiceFactory for `quinn`.
pub struct H3ServiceBuilder<F> {
    factory: F,
}

impl<F, B, E> H3ServiceBuilder<F>
where
    F: ServiceFactory<Request<RequestBody>, Response = Response<ResponseBody<B>>>,
    F::Service: 'static,

    B: Stream<Item = Result<Bytes, E>> + 'static,
    E: 'static,
{
    /// Construct a new Service Builder with given service factory.
    pub fn new(factory: F) -> Self {
        Self { factory }
    }
}

impl<F, Arg, ResB, BE> ServiceFactory<UdpStream, Arg> for H3ServiceBuilder<F>
where
    F: ServiceFactory<Request<RequestBody>, Arg, Response = Response<ResponseBody<ResB>>>,
    F::Service: 'static,
    F::Error: fmt::Debug,

    ResB: Stream<Item = Result<Bytes, BE>>,
    BE: fmt::Debug,
{
    type Response = ();
    type Error = HttpServiceError<F::Error, BE>;
    type Service = H3Service<F::Service>;
    type Future = impl Future<Output = Result<Self::Service, Self::Error>>;

    fn new_service(&self, arg: Arg) -> Self::Future {
        let service = self.factory.new_service(arg);
        async {
            let service = service.await.map_err(HttpServiceError::Service)?;
            Ok(H3Service::new(service))
        }
    }
}
