use integration::pb::{test_client::TestClient, test_server::TestServer, Input};
use integration::Svc;
use tokio::{
    stream::{self, StreamExt},
    time::Duration,
    try_join,
};
use tonic::transport::Server;
use tonic_web::GrpcWeb;

#[tokio::test]
async fn smoke() {
    let addr1 = ([127, 0, 0, 1], 1234).into();
    let addr2 = ([127, 0, 0, 1], 1235).into();

    let grpc = TestServer::new(Svc);
    let grpc_web = GrpcWeb::new(grpc.clone());

    let _ = tokio::spawn(async move {
        Server::builder()
            .add_service(grpc)
            .serve(addr1)
            .await
            .unwrap();
    });

    let _ = tokio::spawn(async move {
        Server::builder()
            .add_service(grpc_web)
            .serve(addr2)
            .await
            .unwrap();
    });

    tokio::time::delay_for(Duration::from_millis(30)).await;

    let (mut client1, mut client2) = try_join!(
        TestClient::connect(format!("http://{}", addr1)),
        TestClient::connect(format!("http://{}", addr2))
    )
    .unwrap();

    let input = Input {
        id: 1,
        desc: "one".to_owned(),
    };

    let (res1, res2) = try_join!(
        client1.unary_call(input.clone()),
        client2.unary_call(input.clone())
    )
    .unwrap();

    assert_eq!(
        format!("{:?}", res1.metadata()),
        format!("{:?}", res2.metadata())
    );

    assert_eq!(res1.into_inner(), res2.into_inner());

    let (res3, res4) = try_join!(
        client1.server_stream(input.clone()),
        client2.server_stream(input.clone())
    )
    .unwrap();

    assert_eq!(
        format!("{:?}", res3.metadata()),
        format!("{:?}", res4.metadata())
    );

    assert_eq!(
        res3.into_inner()
            .collect::<Result<Vec<_>, _>>()
            .await
            .unwrap(),
        res4.into_inner()
            .collect::<Result<Vec<_>, _>>()
            .await
            .unwrap()
    );

    let input = vec![input.clone(), input.clone()];

    let (res5, res6) = try_join!(
        client1.client_stream(stream::iter(input.clone())),
        client2.client_stream(stream::iter(input))
    )
    .unwrap();

    assert_eq!(
        format!("{:?}", res5.metadata()),
        format!("{:?}", res6.metadata())
    );

    assert_eq!(res5.into_inner(), res6.into_inner());

    let input = Input {
        id: 2,
        desc: "boom".to_owned(),
    };

    let (res7, res8) = tokio::join!(
        client1.unary_call(input.clone()),
        client2.unary_call(input.clone())
    );

    let status1 = res7.unwrap_err();
    let status2 = res8.unwrap_err();

    assert_eq!(status1.code(), status2.code());
    assert_eq!(status1.message(), status2.message());
    assert_eq!(
        format!("{:?}", status1.metadata()),
        format!("{:?}", status2.metadata())
    );
}
