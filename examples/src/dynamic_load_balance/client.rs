pub mod pb {
    tonic::include_proto!("grpc.examples.unaryecho");
}

use pb::{echo_client::EchoClient, EchoRequest};
use tonic::transport::Channel;

use tonic::transport::Endpoint;

use std::sync::Arc;

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use tokio::time::timeout;
use tower::discover::Change;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let e1 = Endpoint::from_static("http://[::1]:50051");
    let e2 = Endpoint::from_static("http://[::1]:50052");

    let (channel, rx) = Channel::balance_channel(10);
    let mut client = EchoClient::new(channel);

    let done = Arc::new(AtomicBool::new(false));
    let demo_done = done.clone();
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        println!("Added first endpoint");
        let change = Change::Insert("1", e1);
        let res = rx.send(change).await;
        println!("{:?}", res);
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        println!("Added second endpoint");
        let change = Change::Insert("2", e2);
        let res = rx.send(change).await;
        println!("{:?}", res);
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        println!("Removed first endpoint");
        let change = Change::Remove("1");
        let res = rx.send(change).await;
        println!("{:?}", res);

        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        println!("Removed second endpoint");
        let change = Change::Remove("2");
        let res = rx.send(change).await;
        println!("{:?}", res);

        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        println!("Added third endpoint");
        let e3 = Endpoint::from_static("http://[::1]:50051");
        let change = Change::Insert("3", e3);
        let res = rx.send(change).await;
        println!("{:?}", res);

        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        println!("Removed third endpoint");
        let change = Change::Remove("3");
        let res = rx.send(change).await;
        println!("{:?}", res);
        demo_done.swap(true, SeqCst);
    });

    while !done.load(SeqCst) {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        let request = tonic::Request::new(EchoRequest {
            message: "hello".into(),
        });

        let rx = client.unary_echo(request);
        if let Ok(resp) = timeout(tokio::time::Duration::from_secs(10), rx).await {
            println!("RESPONSE={:?}", resp);
        } else {
            println!("did not receive value within 10 secs");
        }
    }

    println!("... Bye");

    Ok(())
}
