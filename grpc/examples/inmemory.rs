/*
 *
 * Copyright 2025 gRPC authors.
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

use bytes::Buf;
use bytes::Bytes;
use grpc::client;
use grpc::client::CallOptions;
use grpc::client::Channel;
use grpc::client::ChannelOptions;
use grpc::client::Invoke;
use grpc::client::RecvStream as _;
use grpc::client::SendStream as _;
use grpc::core::ClientResponseStreamItem;
use grpc::core::RecvMessage;
use grpc::core::RequestHeaders;
use grpc::core::SendMessage;
use grpc::core::ServerResponseStreamItem;
use grpc::core::Trailers;
use grpc::credentials::InsecureChannelCredentials;
use grpc::inmemory;
use grpc::server;
use grpc::server::Handle;

struct Handler {
    id: String,
}

#[derive(Debug, Default)]
struct MyReqMessage(String);

impl SendMessage for MyReqMessage {
    fn encode(&self) -> Result<Box<dyn Buf + Send + Sync>, String> {
        Ok(Box::new(Bytes::from(self.0.clone())))
    }
}
impl RecvMessage for MyReqMessage {
    fn decode(&mut self, data: &mut dyn Buf) -> Result<(), String> {
        let b = data.copy_to_bytes(data.remaining());
        self.0 = String::from_utf8(b.to_vec()).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[derive(Debug, Default)]
struct MyResMessage(String);
impl SendMessage for MyResMessage {
    fn encode(&self) -> Result<Box<dyn Buf + Send + Sync>, String> {
        Ok(Box::new(Bytes::from(self.0.clone())))
    }
}
impl RecvMessage for MyResMessage {
    fn decode(&mut self, data: &mut dyn Buf) -> Result<(), String> {
        let b = data.copy_to_bytes(data.remaining());
        self.0 = String::from_utf8(b.to_vec()).map_err(|e| e.to_string())?;
        Ok(())
    }
}

impl Handle for Handler {
    async fn handle(
        &self,
        headers: RequestHeaders,
        _options: CallOptions,
        tx: &mut impl server::SendStream,
        mut rx: impl server::RecvStream + 'static,
    ) -> Trailers {
        let method = headers.method_name().clone();
        let id = self.id.clone();
        // Send headers
        let _ = tx
            .send(
                ServerResponseStreamItem::Headers(grpc::core::ResponseHeaders::default()),
                server::SendOptions::default(),
            )
            .await;

        let mut req_msg = MyReqMessage::default();
        while let Some(Ok(())) = rx.next(&mut req_msg).await {
            let res_msg = MyResMessage(format!(
                "Server {}: responding to: {}; msg: {}",
                id, method, req_msg.0,
            ));
            let _ = tx
                .send(
                    ServerResponseStreamItem::Message(&res_msg),
                    server::SendOptions::default(),
                )
                .await;
        }
        // Return trailers
        Trailers::new(Ok(()))
    }
}

#[tokio::main]
async fn main() {
    inmemory::reg();
    let mut listeners = Vec::new();
    for _ in 0..3 {
        let lis = inmemory::InMemoryListener::new();
        let mut srv = grpc::server::Server::new();
        srv.set_handler(Handler { id: lis.id() });
        let lis_clone = lis.clone();
        tokio::task::spawn(async move {
            srv.serve(&lis_clone).await;
            println!("serve returned for listener {}!", lis_clone.id());
        });
        listeners.push(lis);
    }

    let ids: Vec<String> = listeners.iter().map(|lis| lis.id()).collect();
    let target = format!("inmemory:///{}", ids.join(","));
    println!("Creating channel for {target}");
    let chan_opts = ChannelOptions::default();
    let chan = Channel::new(
        target.as_str(),
        InsecureChannelCredentials::new_arc(),
        chan_opts,
    );

    let expected_servers: std::collections::HashSet<_> = ids.into_iter().collect();
    let mut responding_servers = std::collections::HashSet::new();
    let start = std::time::Instant::now();
    while responding_servers != expected_servers
        && start.elapsed() < std::time::Duration::from_secs(3)
    {
        let server_id = run_rpc(&chan).await;
        if !server_id.is_empty() {
            responding_servers.insert(server_id);
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    println!("Responding servers: {:?}", responding_servers);
    assert_eq!(responding_servers, expected_servers);

    drop(chan);

    for lis in listeners {
        lis.close().await;
    }
}

async fn run_rpc(chan: &Channel) -> String {
    let (mut tx, mut rx) = chan
        .invoke(
            RequestHeaders::new().with_method_name("/some/method"),
            CallOptions::default(),
        )
        .await;

    tokio::spawn(async move {
        let reqs = vec![
            MyReqMessage("My Request 1".to_string()),
            MyReqMessage("My Request 2".to_string()),
            MyReqMessage("My Request 3".to_string()),
        ];

        for req in reqs {
            tx.send(&req, client::SendOptions::default()).await.unwrap();
        }
    });

    let mut server_id = String::new();
    loop {
        let mut res = MyResMessage::default();
        match rx.next(&mut res).await {
            ClientResponseStreamItem::Headers(_) => continue,
            ClientResponseStreamItem::Message => {
                println!("CALL RESPONSE: {}", res.0);
                if let Some(id) = res
                    .0
                    .strip_prefix("Server ")
                    .and_then(|s| s.split(':').next())
                {
                    server_id = id.to_string();
                }
            }
            ClientResponseStreamItem::Trailers(trailers) => {
                assert!(trailers.status().is_ok());
            }
            ClientResponseStreamItem::StreamClosed => break,
        }
    }
    server_id
}
