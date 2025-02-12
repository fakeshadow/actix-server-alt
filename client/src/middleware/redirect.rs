use crate::{
    error::{Error, InvalidUri},
    http::{
        header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, LOCATION, TRANSFER_ENCODING},
        Method, Request, StatusCode, Uri,
    },
    response::Response,
    service::{Service, ServiceRequest},
};

/// middleware for following redirect response.
pub struct FollowRedirect<S> {
    service: S,
}

impl<S> FollowRedirect<S> {
    pub fn new(service: S) -> Self {
        Self { service }
    }
}

impl<'c, S> Service<ServiceRequest<'c>> for FollowRedirect<S>
where
    S: for<'c2> Service<ServiceRequest<'c2>, Response = Response, Error = Error> + Send + Sync,
{
    type Response = Response;
    type Error = Error;

    async fn call(&self, req: ServiceRequest<'c>) -> Result<Self::Response, Self::Error> {
        let ServiceRequest { req, client, timeout } = req;
        let (mut head, mut body) = req.into_parts();

        loop {
            let body = core::mem::take(&mut body);
            let req = Request::from_parts(head.clone(), body);
            let mut res = self.service.call(ServiceRequest { req, client, timeout }).await?;
            match res.status() {
                StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER => {
                    if head.method != Method::HEAD {
                        head.method = Method::GET;
                    }

                    for header in &[TRANSFER_ENCODING, CONTENT_ENCODING, CONTENT_TYPE, CONTENT_LENGTH] {
                        head.headers.remove(header);
                    }
                }
                StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => {}
                _ => return Ok(res),
            };

            let Some(location) = res.headers_mut().remove(LOCATION) else {
                return Ok(res);
            };

            let parts = core::mem::take(&mut head.uri).into_parts();

            let parts_location = location
                .to_str()
                .map_err(|_| InvalidUri::MissingPathQuery)?
                .parse::<Uri>()?
                .into_parts();

            let mut uri_builder = Uri::builder();

            if let Some(a) = parts_location.authority.or(parts.authority) {
                uri_builder = uri_builder.authority(a);
            }

            if let Some(s) = parts_location.scheme.or(parts.scheme) {
                uri_builder = uri_builder.scheme(s);
            }

            let path = parts_location.path_and_query.ok_or(InvalidUri::MissingPathQuery)?;
            head.uri = uri_builder.path_and_query(path).build().unwrap();
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{
        body::ResponseBody,
        http,
        service::{mock_service, Service},
    };

    use super::*;

    #[tokio::test]
    async fn redirect() {
        let (handle, service) = mock_service();

        let redirect = FollowRedirect::new(service);

        let req = http::Request::builder()
            .uri("http://foo.bar/foo")
            .body(Default::default())
            .unwrap();

        let req = handle.mock(req, |req| match req.uri().path() {
            "/foo" => Ok(http::Response::builder()
                .status(StatusCode::SEE_OTHER)
                .header("location", "/bar")
                .body(ResponseBody::Eof)
                .unwrap()),
            "/bar" => Ok(http::Response::builder()
                .status(StatusCode::IM_A_TEAPOT)
                .body(ResponseBody::Eof)
                .unwrap()),
            p => panic!("unexpected uri path: {p}"),
        });

        let res = redirect.call(req).await.unwrap();

        assert_eq!(res.status(), StatusCode::IM_A_TEAPOT);
    }
}
