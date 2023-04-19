use std::convert::Infallible;
use std::net::SocketAddr;

use axum::error_handling::HandleErrorLayer;
use base64::Engine as _;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use hyper::http::{header, StatusCode};
use hyper::server::conn::AddrIncoming;
use hyper::service::make_service_fn;
use hyper::{Body, Client, Method, Request, Uri};
use prost::Message;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use tower::util::MapRequestLayer;
use tower::ServiceBuilder;

use integration::pb::{test_server::TestServer, Input, Output};
use integration::Svc;
use tonic_web::GrpcWebLayer;

#[tokio::test]
async fn binary_request() {
    let server_url = spawn().await;
    let client = Client::new();

    let req = build_request(server_url, "grpc-web", "grpc-web");
    let res = client.request(req).await.unwrap();
    let content_type = res.headers().get(header::CONTENT_TYPE).unwrap().clone();
    let content_type = content_type.to_str().unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(content_type, "application/grpc-web+proto");

    let (message, trailers) = decode_body(res.into_body(), content_type).await;
    let expected = Output {
        id: 1,
        desc: "one".to_owned(),
    };

    assert_eq!(message, expected);
    assert_eq!(&trailers[..], b"grpc-status:0\r\n");
}

#[tokio::test]
async fn binary_request_reverse_proxy() {
    let server_url = spawn_reverse_proxy().await;
    let client = Client::new();

    let req = build_request(server_url, "grpc-web", "grpc-web");
    let res = client.request(req).await.unwrap();
    let content_type = res.headers().get(header::CONTENT_TYPE).unwrap().clone();
    let content_type = content_type.to_str().unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(content_type, "application/grpc-web+proto");

    let (message, trailers) = decode_body(res.into_body(), content_type).await;
    let expected = Output {
        id: 1,
        desc: "one".to_owned(),
    };

    assert_eq!(message, expected);
    assert_eq!(&trailers[..], b"grpc-status:0\r\n");
}

#[tokio::test]
async fn text_request() {
    let server_url = spawn().await;
    let client = Client::new();

    let req = build_request(server_url, "grpc-web-text", "grpc-web-text");
    let res = client.request(req).await.unwrap();
    let content_type = res.headers().get(header::CONTENT_TYPE).unwrap().clone();
    let content_type = content_type.to_str().unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(content_type, "application/grpc-web-text+proto");

    let (message, trailers) = decode_body(res.into_body(), content_type).await;
    let expected = Output {
        id: 1,
        desc: "one".to_owned(),
    };

    assert_eq!(message, expected);
    assert_eq!(&trailers[..], b"grpc-status:0\r\n");
}

async fn spawn() -> String {
    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = TcpListener::bind(addr).await.expect("listener");
    let url = format!("http://{}", listener.local_addr().unwrap());
    let listener_stream = TcpListenerStream::new(listener);

    let _ = tokio::spawn(async move {
        Server::builder()
            .accept_http1(true)
            .layer(GrpcWebLayer::new())
            .add_service(TestServer::new(Svc))
            .serve_with_incoming(listener_stream)
            .await
            .unwrap()
    });

    url
}

/// Spawn two servers, one serving the gRPC API
async fn spawn_reverse_proxy() -> String {
    // Set up gRPC service
    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = TcpListener::bind(addr).await.expect("listener");
    let url = format!("http://{}", listener.local_addr().unwrap());
    let listener_stream = TcpListenerStream::new(listener);

    let _ = tokio::spawn(async move {
        Server::builder()
            .add_service(TestServer::new(Svc))
            .serve_with_incoming(listener_stream)
            .await
            .unwrap()
    });

    // Set up proxy to the above service that applies tonic-web
    let addr2 = SocketAddr::from(([127, 0, 0, 1], 0));
    let http_client = hyper::Client::builder().http2_only(true).build_http();
    let listener2 = TcpListener::bind(addr2).await.expect("listener");
    let url2 = format!("http://{}", listener2.local_addr().unwrap());

    let svc = ServiceBuilder::new()
        .layer(GrpcWebLayer::new())
        .layer(MapRequestLayer::new(move |r: hyper::Request<_>| {
            let (mut parts, body) = r.into_parts();
            parts.uri = format!("{}{}", url, parts.uri).parse().unwrap();
            Request::from_parts(parts, body)
        }))
        .layer(HandleErrorLayer::new(|_err| async {
            Ok::<_, Infallible>((StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"))
        }))
        .service(http_client);
    let make_svc = make_service_fn(move |_| {
        let svc = svc.clone();
        async { Ok::<_, Infallible>(svc) }
    });

    let _ = tokio::spawn(async move {
        hyper::Server::builder(AddrIncoming::from_listener(listener2).unwrap())
            .serve(make_svc)
            .await
            .unwrap()
    });

    url2
}

fn encode_body() -> Bytes {
    let input = Input {
        id: 1,
        desc: "one".to_owned(),
    };

    let mut buf = BytesMut::with_capacity(1024);
    buf.reserve(5);
    unsafe {
        buf.advance_mut(5);
    }

    input.encode(&mut buf).unwrap();

    let len = buf.len() - 5;
    {
        let mut buf = &mut buf[..5];
        buf.put_u8(0);
        buf.put_u32(len as u32);
    }

    buf.split_to(len + 5).freeze()
}

fn build_request(base_uri: String, content_type: &str, accept: &str) -> Request<Body> {
    use header::{ACCEPT, CONTENT_TYPE, ORIGIN};

    let request_uri = format!("{}/{}/{}", base_uri, "test.Test", "UnaryCall")
        .parse::<Uri>()
        .unwrap();

    let bytes = match content_type {
        "grpc-web" => encode_body(),
        "grpc-web-text" => integration::util::base64::STANDARD
            .encode(encode_body())
            .into(),
        _ => panic!("invalid content type {}", content_type),
    };

    Request::builder()
        .method(Method::POST)
        .header(CONTENT_TYPE, format!("application/{}", content_type))
        .header(ORIGIN, "http://example.com")
        .header(ACCEPT, format!("application/{}", accept))
        .uri(request_uri)
        .body(Body::from(bytes))
        .unwrap()
}

async fn decode_body(body: Body, content_type: &str) -> (Output, Bytes) {
    let mut body = hyper::body::to_bytes(body).await.unwrap();

    if content_type == "application/grpc-web-text+proto" {
        body = integration::util::base64::STANDARD
            .decode(body)
            .unwrap()
            .into()
    }

    body.advance(1);
    let len = body.get_u32();
    let msg = Output::decode(&mut body.split_to(len as usize)).expect("decode");
    body.advance(5);

    (msg, body)
}
