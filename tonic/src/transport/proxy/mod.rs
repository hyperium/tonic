mod cors;

use crate::body::BoxBody;
use crate::transport::server::Svc;
use futures_util::{FutureExt, TryFutureExt};
use http::{Request, Response, StatusCode};
use hyper::Body;
use pretty_hex::*;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::Service;

pub(crate) struct ProxySvc<S> {
    pub(crate) config: ProxyConfig,
    pub(crate) inner: Svc<S>,
}
impl<S> Service<Request<Body>> for ProxySvc<S>
where
    S: Service<Request<Body>, Response = Response<BoxBody>> + Send,
    S::Future: Send + 'static,
    S::Error: Into<crate::Error> + 'static,
{
    type Response = Response<BoxBody>;
    type Error = crate::Error;
    //type Future = MapErr<Instrumented<S::Future>, fn(S::Error) -> crate::Error>;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        //TODO: Remove me
        println!("\n\n\nRequest: {:?}", req);

        //If it's a HTTP/2 request, let it through.
        if req.version() == http::Version::HTTP_2 {
            return Box::pin(self.inner.call(req));
        }

        //Error it the request is not Http/2 or Http1/1 (Should support 1.0)
        if req.version() != http::Version::HTTP_11 {
            return Box::pin(async {
                let response = http::Response::builder()
                    .status(500)
                    .body(BoxBody::empty())
                    .unwrap();
                Ok(response)
            });
        }

        //Get the CORs state
        let cors_state = self.config.cors_config.process_request(&req);
        let uri = req.uri().to_string();
        let version = req.version();

        //Handle the simple CORs cases.
        match cors_state {
            Ok(cors::CorsResource::Preflight(headers)) => {
                let mut response = http::Response::new(BoxBody::empty());
                *response.status_mut() = StatusCode::NO_CONTENT;
                *response.headers_mut() = headers;
                return Box::pin(async { Ok(response) });
            }
            Err(e) => {
                let mut response = http::Response::new(BoxBody::empty());
                *response.status_mut() = StatusCode::FORBIDDEN;
                return Box::pin(async { Ok(response) });
            }
            _ => {}
        }

        //Update the version
        let version = req.version_mut();
        *version = http::Version::HTTP_2;

        //Get headers and transform stuff.
        let (mut parts, body): (_, Body) = req.into_parts();

        //let content_type = parts.headers.get("content-type").unwrap().to_str().unwrap();
        let user_agent = parts.headers.get("x-user-agent").unwrap().to_str().unwrap();
        let origin = parts
            .headers
            .get("origin")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        parts
            .headers
            .insert("user-agent", user_agent.parse().unwrap());

        let body = hyper::body::Body::wrap_stream(
            hyper::body::to_bytes(body)
                .and_then(|x| {
                    let decoded = base64::decode_config(&x, base64::STANDARD).unwrap();
                    println!("Body Decoded: {}", decoded.hex_dump());
                    futures_util::future::ok(bytes::Bytes::from(decoded))
                })
                .into_stream(),
        );

        let req = http::Request::from_parts(parts, body);

        println!("\nTransformed: {:?}", req);

        let fut = self.inner.call(req);
        let fut = async move {
            let mut response: http::Response<BoxBody> = fut.await.unwrap();
            //Modify the body
            let version = response.version_mut();
            *version = http::Version::HTTP_11;

            let (mut parts, body) = response.into_parts();

            let body = hyper::body::Body::wrap_stream(
                hyper::body::to_bytes(body)
                    .and_then(|x: bytes::Bytes| {
                        println!("Response Body: \n {}", x.as_ref().hex_dump());
                        let encoded = base64::encode_config(&x, base64::STANDARD);

                        futures_util::future::ok(bytes::Bytes::from(encoded))
                    })
                    .into_stream(),
            );

            //Need to insert the CORs headers in.
            if let Ok(cors::CorsResource::Simple(headers)) = cors_state {
                parts.headers.extend(headers);
            }

            //I saw these two GRPC headers, when watching the envoy requests. I'm not sure what they do, quite yet.
            //TODO: Investigate the two headers below and understand when to insert them.
            parts
                .headers
                .insert("grpc-accept-encoding", "identity".parse().unwrap());
            parts
                .headers
                .insert("grpc-encoding", "identity".parse().unwrap());
            parts.headers.insert(
                "content-type",
                "application/grpc-web-text+proto".parse().unwrap(),
            );
            parts.headers.insert("server", "mine".parse().unwrap());
            let response = http::Response::from_parts(parts, BoxBody::map_from(body));
            println!("\nResponse: {:?}", response);
            Ok(response)
        };
        Box::pin(fut)
    }
}

#[derive(Clone)]
pub struct ProxyConfig {
    pub cors_config: cors::Config,
}
impl Default for ProxyConfig {
    fn default() -> Self {
        use http::header::HeaderName;

        //TODO: Probably dont need all these headers. Need to find out the min set.
        let headers = [
            HeaderName::from_lowercase(b"keep-alive").unwrap(),
            HeaderName::from_lowercase(b"user-agent").unwrap(),
            HeaderName::from_lowercase(b"cache-control").unwrap(),
            HeaderName::from_lowercase(b"content-type").unwrap(),
            HeaderName::from_lowercase(b"content-transfer-encoding").unwrap(),
            HeaderName::from_lowercase(b"custom-header-1").unwrap(),
            HeaderName::from_lowercase(b"x-accept-content-transfer-encoding").unwrap(),
            HeaderName::from_lowercase(b"x-accept-response-streaming").unwrap(),
            HeaderName::from_lowercase(b"x-user-agent").unwrap(),
            HeaderName::from_lowercase(b"x-grpc-web").unwrap(),
            HeaderName::from_lowercase(b"grpc-timeout").unwrap(),
        ];
        let expose_headers = [
            HeaderName::from_lowercase(b"custom-header-1").unwrap(),
            HeaderName::from_lowercase(b"grpc-status").unwrap(),
            HeaderName::from_lowercase(b"grpc-message").unwrap(),
        ];

        let cors_builder = cors::CorsBuilder::new()
            .allow_credentials(false)
            .allow_origins(cors::AllowedOrigins::Any { allow_null: false })
            .allow_methods([http::Method::GET, http::Method::POST].iter())
            .allow_headers(headers.iter())
            .expose_headers(expose_headers.iter());
        Self {
            cors_config: cors_builder.into_config(),
        }
    }
}
