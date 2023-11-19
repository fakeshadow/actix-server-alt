mod object;

use core::{
    cell::RefCell,
    convert::Infallible,
    fmt,
    future::{ready, Future, Ready},
};

use futures_core::stream::Stream;
use xitca_http::util::{
    middleware::context::{Context, ContextBuilder},
    service::router::{IntoObject, PathGen, Router, SyncMarker},
};

use crate::{
    body::ResponseBody,
    bytes::Bytes,
    dev::service::{ready::ReadyService, AsyncClosure, EnclosedFactory, EnclosedFnFactory, Service, ServiceExt},
    handler::Responder,
    http::{Request, RequestExt},
    request::WebRequest,
    response::WebResponse,
    server::HttpServer,
};

/// composed application type with router, stateful context and default middlewares.
pub struct App<CF = (), R = ()> {
    ctx_factory: CF,
    router: R,
}

impl App {
    /// Construct a new application instance.
    pub fn new<Obj>() -> App<impl Fn() -> Ready<Result<(), Infallible>>, Router<Obj>> {
        Self::with_async_state(|| ready(Ok(())))
    }

    /// Construct App with a thread safe state.
    ///
    /// State would be shared among all tasks and worker threads.
    pub fn with_state<C, Obj>(state: C) -> App<impl Fn() -> Ready<Result<C, Infallible>>, Router<Obj>>
    where
        C: Send + Sync + Clone + 'static,
    {
        Self::with_async_state(move || ready(Ok(state.clone())))
    }

    #[doc(hidden)]
    /// Construct App with async closure which it's output would be used as state.
    pub fn with_async_state<CF, Obj>(ctx_factory: CF) -> App<CF, Router<Obj>>
where {
        App {
            ctx_factory,
            router: Router::new(),
        }
    }
}

impl<CF, Obj> App<CF, Router<Obj>> {
    /// insert routed service with given path to application.
    pub fn at<Fut, C, E, F, B>(mut self, path: &'static str, factory: F) -> App<CF, Router<Obj>>
    where
        CF: Fn() -> Fut,
        Fut: Future<Output = Result<C, E>>,
        F: PathGen + Service + Send + Sync,
        F::Response: for<'r> Service<WebRequest<'r, C, B>>,
        for<'r> WebRequest<'r, C, B>: IntoObject<F, (), SyncMarker, Object = Obj>,
    {
        self.router = self.router.insert_sync(path, factory);
        self
    }
}

impl<CF, R> App<CF, R>
where
    R: Service,
{
    /// Enclose App with middleware type.
    /// Middleware must impl [Service] trait.
    pub fn enclosed<T>(self, transform: T) -> App<CF, EnclosedFactory<R, T>>
    where
        T: Service<R::Response>,
    {
        App {
            ctx_factory: self.ctx_factory,
            router: self.router.enclosed(transform),
        }
    }

    /// Enclose App with function as middleware type.
    pub fn enclosed_fn<Req, T>(self, transform: T) -> App<CF, EnclosedFnFactory<R, T>>
    where
        T: for<'s> AsyncClosure<(&'s R::Response, Req)> + Clone,
    {
        App {
            ctx_factory: self.ctx_factory,
            router: self.router.enclosed_fn(transform),
        }
    }

    /// Finish App build. No other App method can be called afterwards.
    pub fn finish<C, Fut, CErr, ReqB, ResB, E, Err>(
        self,
    ) -> impl Service<
        Response = impl ReadyService
                       + Service<Request<RequestExt<ReqB>>, Response = WebResponse<ResponseBody<ResB>>, Error = Err>,
        Error = impl fmt::Debug,
    >
    where
        CF: Fn() -> Fut,
        Fut: Future<Output = Result<C, CErr>>,
        C: 'static,
        CErr: fmt::Debug,
        ReqB: 'static,
        R::Response: ReadyService + for<'r> Service<WebRequest<'r, C, ReqB>, Response = WebResponse<ResB>, Error = Err>,
        R::Error: fmt::Debug,
        Err: for<'r> Responder<WebRequest<'r, C, ReqB>, Output = WebResponse>,
        ResB: Stream<Item = Result<Bytes, E>>,
    {
        let App { ctx_factory, router } = self;
        router
            .enclosed_fn(map_response)
            .enclosed_fn(map_request)
            .enclosed(ContextBuilder::new(ctx_factory))
    }

    /// Finish App build and serve is with [HttpServer]. No other App method can be called afterwards.
    pub fn serve<C, Fut, CErr, ReqB, ResB, E, Err>(
        self,
    ) -> HttpServer<
        impl Service<
            Response = impl ReadyService
                           + Service<Request<RequestExt<ReqB>>, Response = WebResponse<ResponseBody<ResB>>, Error = Err>,
            Error = impl fmt::Debug,
        >,
    >
    where
        R: Send + Sync + 'static,
        CF: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<C, CErr>> + 'static,
        C: 'static,
        CErr: fmt::Debug + 'static,
        ReqB: 'static,
        R::Response: ReadyService + for<'r> Service<WebRequest<'r, C, ReqB>, Response = WebResponse<ResB>, Error = Err>,
        R::Error: fmt::Debug,
        Err: for<'r> Responder<WebRequest<'r, C, ReqB>, Output = WebResponse> + 'static,
        ResB: Stream<Item = Result<Bytes, E>> + 'static,
        E: 'static,
    {
        HttpServer::serve(self.finish())
    }
}

async fn map_response<B, C, S, ResB, E, Err>(
    service: &S,
    mut req: WebRequest<'_, C, B>,
) -> Result<WebResponse<ResponseBody<ResB>>, Err>
where
    C: 'static,
    B: 'static,
    S: for<'r> Service<WebRequest<'r, C, B>, Response = WebResponse<ResB>, Error = Err>,
    Err: for<'r> Responder<WebRequest<'r, C, B>, Output = WebResponse>,
    ResB: Stream<Item = Result<Bytes, E>>,
{
    match service.call(req.reborrow()).await {
        Ok(res) => Ok(res.map(|body| ResponseBody::stream(body))),
        // TODO: mutate response header according to outcome of drop_stream_cast?
        Err(e) => Ok(e.respond_to(req).await.map(|body| body.drop_stream_cast())),
    }
}

async fn map_request<B, C, S, Res, Err>(service: &S, req: Context<'_, Request<RequestExt<B>>, C>) -> Result<Res, Err>
where
    C: 'static,
    B: 'static,
    S: for<'r> Service<WebRequest<'r, C, B>, Response = Res, Error = Err>,
{
    let (req, state) = req.into_parts();
    let (parts, ext) = req.into_parts();
    let (ext, body) = ext.replace_body(());
    let mut req = Request::from_parts(parts, ext);
    let mut body = RefCell::new(body);
    let req = WebRequest::new(&mut req, &mut body, state);
    service.call(req).await
}

#[cfg(test)]
mod test {
    use std::{
        pin::Pin,
        task::{self, Poll},
    };

    use xitca_unsafe_collection::futures::NowOrPanic;

    use crate::{
        dev::service::Service,
        handler::{
            extension::ExtensionRef, extension::ExtensionsRef, handler_service, path::PathRef, state::StateRef,
            uri::UriRef, Responder,
        },
        http::{const_header_value::TEXT_UTF8, header::CONTENT_TYPE, Method, Uri},
        middleware::UncheckedReady,
        request::RequestBody,
        route::get,
    };

    use super::*;

    async fn handler(
        StateRef(state): StateRef<'_, String>,
        PathRef(path): PathRef<'_>,
        UriRef(_): UriRef<'_>,
        ExtensionRef(_): ExtensionRef<'_, Foo>,
        ExtensionsRef(_): ExtensionsRef<'_>,
        req: &WebRequest<'_, String, NewBody<RequestBody>>,
    ) -> String {
        assert_eq!("state", state);
        assert_eq!(state, req.state());
        assert_eq!("/", path);
        assert_eq!(path, req.req().uri().path());
        state.to_string()
    }

    // Handler with no state extractor
    async fn stateless_handler(_: PathRef<'_>) -> String {
        String::from("debug")
    }

    #[derive(Clone)]
    struct Middleware;

    impl<S> Service<S> for Middleware {
        type Response = MiddlewareService<S>;
        type Error = Infallible;

        async fn call(&self, service: S) -> Result<Self::Response, Self::Error> {
            Ok(MiddlewareService(service))
        }
    }

    struct MiddlewareService<S>(S);

    impl<'r, S, C, B, Res, Err> Service<WebRequest<'r, C, B>> for MiddlewareService<S>
    where
        S: for<'r2> Service<WebRequest<'r2, C, B>, Response = Res, Error = Err>,
        C: 'r,
        B: 'r,
    {
        type Response = Res;
        type Error = Err;

        async fn call(&self, mut req: WebRequest<'r, C, B>) -> Result<Self::Response, Self::Error> {
            self.0.call(req.reborrow()).await
        }
    }

    // arbitrary body type mutation
    struct NewBody<B>(B);

    impl<B> Stream for NewBody<B>
    where
        B: Stream + Unpin,
    {
        type Item = B::Item;

        fn poll_next(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
            Pin::new(&mut self.get_mut().0).poll_next(cx)
        }
    }

    #[allow(clippy::borrow_interior_mutable_const)]
    #[test]
    fn test_app() {
        async fn middleware_fn<S, C, B, Res, Err>(service: &S, mut req: WebRequest<'_, C, B>) -> Result<Res, Infallible>
        where
            S: for<'r> Service<WebRequest<'r, C, NewBody<B>>, Response = Res, Error = Err>,
            B: Default,
            Err: for<'r> Responder<WebRequest<'r, C, B>, Output = Res>,
        {
            let body = &mut RefCell::new(NewBody(req.take_body_mut()));
            let req2 = WebRequest {
                req: req.req,
                body,
                ctx: req.ctx,
            };
            match service.call(req2).await {
                Ok(res) => Ok(res),
                Err(e) => Ok(e.respond_to(req).await),
            }
        }

        let state = String::from("state");

        let service = App::with_state(state)
            .at("/", get(handler_service(handler)))
            .at(
                "/stateless",
                get(handler_service(stateless_handler)).head(handler_service(stateless_handler)),
            )
            .enclosed_fn(middleware_fn)
            .enclosed(Middleware)
            .enclosed(UncheckedReady)
            .finish()
            .call(())
            .now_or_panic()
            .ok()
            .unwrap();

        let mut req = Request::default();
        req.extensions_mut().insert(Foo);

        let res = service.call(req).now_or_panic().unwrap();

        assert_eq!(res.status().as_u16(), 200);

        assert_eq!(res.headers().get(CONTENT_TYPE).unwrap(), TEXT_UTF8);

        let mut req = Request::default();
        *req.uri_mut() = Uri::from_static("/abc");

        let res = service.call(req).now_or_panic().unwrap();

        assert_eq!(res.status().as_u16(), 404);

        let mut req = Request::default();
        *req.method_mut() = Method::POST;

        let res = service.call(req).now_or_panic().unwrap();

        assert_eq!(res.status().as_u16(), 405);
    }

    struct Foo;
}
