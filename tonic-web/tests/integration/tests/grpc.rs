use std::future::Future;
use std::net::SocketAddr;

use tokio::net::TcpListener;
use tokio::time::Duration;
use tokio::{join, try_join};
use tokio_stream::wrappers::TcpListenerStream;
use tokio_stream::{self as stream, StreamExt};
use tonic::transport::{Channel, Error, Server};
use tonic::{Response, Streaming};

use integration::pb::{test_client::TestClient, test_server::TestServer, Input};
use integration::Svc;
use tonic_web::GrpcWebLayer;

#[tokio::test]
async fn smoke_unary() {
    let (mut c1, mut c2, mut c3, mut c4) = spawn().await.expect("clients");

    let (r1, r2, r3, r4) = try_join!(
        c1.unary_call(input()),
        c2.unary_call(input()),
        c3.unary_call(input()),
        c4.unary_call(input()),
    )
    .expect("responses");

    assert!(meta(&r1) == meta(&r2) && meta(&r2) == meta(&r3) && meta(&r3) == meta(&r4));
    assert!(data(&r1) == data(&r2) && data(&r2) == data(&r3) && data(&r3) == data(&r4));
}

#[tokio::test]
async fn smoke_client_stream() {
    let (mut c1, mut c2, mut c3, mut c4) = spawn().await.expect("clients");

    let input_stream = || stream::iter(vec![input(), input()]);

    let (r1, r2, r3, r4) = try_join!(
        c1.client_stream(input_stream()),
        c2.client_stream(input_stream()),
        c3.client_stream(input_stream()),
        c4.client_stream(input_stream()),
    )
    .expect("responses");

    assert!(meta(&r1) == meta(&r2) && meta(&r2) == meta(&r3) && meta(&r3) == meta(&r4));
    assert!(data(&r1) == data(&r2) && data(&r2) == data(&r3) && data(&r3) == data(&r4));
}

#[tokio::test]
async fn smoke_server_stream() {
    let (mut c1, mut c2, mut c3, mut c4) = spawn().await.expect("clients");

    let (r1, r2, r3, r4) = try_join!(
        c1.server_stream(input()),
        c2.server_stream(input()),
        c3.server_stream(input()),
        c4.server_stream(input()),
    )
    .expect("responses");

    assert!(meta(&r1) == meta(&r2) && meta(&r2) == meta(&r3) && meta(&r3) == meta(&r4));

    let r1 = stream(r1).await;
    let r2 = stream(r2).await;
    let r3 = stream(r3).await;
    let r4 = stream(r4).await;

    assert!(r1 == r2 && r2 == r3 && r3 == r4);
}
#[tokio::test]
async fn smoke_error() {
    let (mut c1, mut c2, mut c3, mut c4) = spawn().await.expect("clients");

    let boom = Input {
        id: 1,
        desc: "boom".to_owned(),
    };

    let (r1, r2, r3, r4) = join!(
        c1.unary_call(boom.clone()),
        c2.unary_call(boom.clone()),
        c3.unary_call(boom.clone()),
        c4.unary_call(boom.clone()),
    );

    let s1 = r1.unwrap_err();
    let s2 = r2.unwrap_err();
    let s3 = r3.unwrap_err();
    let s4 = r4.unwrap_err();

    assert!(status(&s1) == status(&s2) && status(&s2) == status(&s3) && status(&s3) == status(&s4))
}

async fn bind() -> (TcpListener, String) {
    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let lis = TcpListener::bind(addr).await.expect("listener");
    let url = format!("http://{}", lis.local_addr().unwrap());

    (lis, url)
}

async fn grpc(accept_h1: bool) -> (impl Future<Output = Result<(), Error>>, String) {
    let (listener, url) = bind().await;

    let fut = Server::builder()
        .accept_http1(accept_h1)
        .add_service(TestServer::new(Svc))
        .serve_with_incoming(TcpListenerStream::new(listener));

    (fut, url)
}

async fn grpc_web(accept_h1: bool) -> (impl Future<Output = Result<(), Error>>, String) {
    let (listener, url) = bind().await;

    let fut = Server::builder()
        .accept_http1(accept_h1)
        .layer(GrpcWebLayer::new())
        .add_service(TestServer::new(Svc))
        .serve_with_incoming(TcpListenerStream::new(listener));

    (fut, url)
}

type Client = TestClient<Channel>;

async fn spawn() -> Result<(Client, Client, Client, Client), Error> {
    let ((s1, u1), (s2, u2), (s3, u3), (s4, u4)) =
        join!(grpc(true), grpc(false), grpc_web(true), grpc_web(false));

    drop(tokio::spawn(async move { join!(s1, s2, s3, s4) }));

    tokio::time::sleep(Duration::from_millis(30)).await;

    try_join!(
        TestClient::connect(u1),
        TestClient::connect(u2),
        TestClient::connect(u3),
        TestClient::connect(u4)
    )
}

fn input() -> Input {
    Input {
        id: 1,
        desc: "one".to_owned(),
    }
}

fn meta<T>(r: &Response<T>) -> String {
    format!("{:?}", r.metadata())
}

fn data<T>(r: &Response<T>) -> &T {
    r.get_ref()
}

async fn stream<T>(r: Response<Streaming<T>>) -> Vec<T> {
    r.into_inner().collect::<Result<Vec<_>, _>>().await.unwrap()
}

fn status(s: &tonic::Status) -> (String, tonic::Code) {
    (format!("{:?}", s.metadata()), s.code())
}
