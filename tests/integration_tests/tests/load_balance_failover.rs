/*
 *
 * Copyright 2026 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use integration_tests::pb::{test_client::TestClient, test_server, Input, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tonic::{
    transport::{server::TcpIncoming, Channel, Endpoint, Server},
    Request, Response, Status,
};

struct Svc(Arc<AtomicUsize>);

#[tonic::async_trait]
impl test_server::Test for Svc {
    async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Response::new(Output {}))
    }
}

#[tokio::test]
async fn balance_list_failover_when_one_endpoint_fails_to_connect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpIncoming::from(listener).with_nodelay(Some(true));

    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    let count = Arc::new(AtomicUsize::new(0));
    let svc = test_server::TestServer::new(Svc(count.clone()));

    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming_shutdown(incoming, async { drop(rx.await) })
            .await
            .unwrap();
    });

    // Endpoint 1: Alive
    let ep_a = Endpoint::from_shared(format!("http://{addr}")).unwrap();
    // Endpoint 2: Dead server with 50ms connect timeout and 50ms reconnect delay
    let ep_b = Endpoint::from_static("http://127.0.0.1:1")
        .connect_timeout(Duration::from_millis(50))
        .reconnect_delay(Duration::from_millis(50));

    let channel = Channel::balance_list(vec![ep_a, ep_b].into_iter());
    let mut client = TestClient::new(channel);

    // Wait for ep_b initial connect to fail
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send requests: all should route to healthy ep_a!
    for i in 0..20 {
        let res = client.unary_call(Request::new(Input {})).await;
        assert!(res.is_ok(), "Request {} failed: {:?}", i, res.err());
    }

    assert_eq!(count.load(Ordering::SeqCst), 20);
    jh.abort();
}

#[tokio::test]
async fn balance_list_reconnects_when_down_endpoint_starts_up() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing_subscriber::filter::LevelFilter::TRACE)
        .try_init();

    // Server A: Running initially
    let listener_a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_a = listener_a.local_addr().unwrap();
    let incoming_a = TcpIncoming::from(listener_a).with_nodelay(Some(true));
    let count_a = Arc::new(AtomicUsize::new(0));

    let jh_a = tokio::spawn(async move {
        Server::builder()
            .add_service(test_server::TestServer::new(Svc(count_a)))
            .serve_with_incoming(incoming_a)
            .await
            .unwrap();
    });

    // Server B: Port allocated, but not started yet
    let listener_b = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_b = listener_b.local_addr().unwrap();
    drop(listener_b); // close listener so connect fails initially

    let ep_a = Endpoint::from_shared(format!("http://{addr_a}")).unwrap();
    let ep_b = Endpoint::from_shared(format!("http://{addr_b}"))
        .unwrap()
        .connect_timeout(Duration::from_millis(50))
        .reconnect_delay(Duration::from_millis(50));

    let channel = Channel::balance_list(vec![ep_a, ep_b].into_iter());
    let mut client = TestClient::new(channel);

    // Initial calls: ep_b is down, all go to ep_a
    for i in 0..10 {
        let res = client.unary_call(Request::new(Input {})).await;
        assert!(res.is_ok(), "Initial request {} failed: {:?}", i, res.err());
    }

    // Now start Server B on the same port!
    let listener_b = TcpListener::bind(addr_b).await.unwrap();
    let incoming_b = TcpIncoming::from(listener_b).with_nodelay(Some(true));
    let count_b = Arc::new(AtomicUsize::new(0));
    let count_b_clone = count_b.clone();

    let jh_b = tokio::spawn(async move {
        Server::builder()
            .add_service(test_server::TestServer::new(Svc(count_b_clone)))
            .serve_with_incoming(incoming_b)
            .await
            .unwrap();
    });

    // Wait for ep_b's backoff to fire and connect to Server B
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Send more requests: now both Server A and Server B should receive requests
    for i in 0..30 {
        let res = client.unary_call(Request::new(Input {})).await;
        assert!(
            res.is_ok(),
            "Subsequent request {} failed: {:?}",
            i,
            res.err()
        );
    }

    // Verify Server B received traffic once it started up!
    let b_requests = count_b.load(Ordering::SeqCst);
    println!("Requests served by Server B after coming up: {b_requests}");
    assert!(
        b_requests > 0,
        "Server B should have received requests after starting up!"
    );

    jh_a.abort();
    jh_b.abort();
}
