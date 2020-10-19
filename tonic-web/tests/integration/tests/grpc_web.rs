use std::net::SocketAddr;

use bytes::{Buf, BufMut, Bytes, BytesMut};
use hyper::http::{header, StatusCode};
use hyper::{Body, Client, Method, Request, Uri};
use prost::Message;
use tokio::net::TcpListener;
use tonic::transport::Server;

use integration::pb::{test_server::TestServer, Input, Output};
use integration::Svc;

#[tokio::test]
async fn binary_request() {
    let server_url = spawn("http://example.com").await;
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
    let server_url = spawn("http://example.com").await;
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

#[tokio::test]
async fn origin_not_allowed() {
    let server_url = spawn("http://foo.com").await;
    let client = Client::new();

    let req = build_request(server_url, "grpc-web-text", "grpc-web-text");
    let res = client.request(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

async fn spawn(allowed_origin: &str) -> String {
    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = TcpListener::bind(addr).await.expect("listener");
    let url = format!("http://{}", listener.local_addr().unwrap());

    let svc = tonic_web::config()
        .allow_origins(vec![allowed_origin])
        .enable(TestServer::new(Svc));

    let _ = tokio::spawn(async move {
        Server::builder()
            .accept_http1(true)
            .add_service(svc)
            .serve_with_incoming(listener)
            .await
            .unwrap()
    });

    url
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
        "grpc-web-text" => base64::encode(encode_body()).into(),
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
        body = base64::decode(body).unwrap().into()
    }

    body.advance(1);
    let len = body.get_u32();
    let msg = Output::decode(&mut body.split_to(len as usize)).expect("decode");
    body.advance(5);

    (msg, body)
}
