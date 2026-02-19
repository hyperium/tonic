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

use std::any::Any;

use tokio_stream::StreamExt;
use tonic::async_trait;

use grpc::client::ChannelOptions;
use grpc::credentials::InsecureChannelCredentials;
use grpc::inmemory;
use grpc::service::Message;
use grpc::service::Request;
use grpc::service::Response;
use grpc::service::Service;

struct Handler {}

#[derive(Debug)]
struct MyReqMessage(String);

#[derive(Debug)]
struct MyResMessage(String);

#[async_trait]
impl Service for Handler {
    async fn call(&self, method: String, request: Request) -> Response {
        let mut stream = request.into_inner();
        let output = async_stream::try_stream! {
            while let Some(req) = stream.next().await {
                yield Box::new(MyResMessage(format!(
                    "Server: responding to: {}; msg: {}",
                    method, (req as Box<dyn Any>).downcast_ref::<MyReqMessage>().unwrap().0,
                ))) as Box<dyn Message>;
            }
        };

        Response::new(Box::pin(output))
    }
}

#[tokio::main]
async fn main() {
    inmemory::reg();

    // Spawn the server.
    let lis = inmemory::Listener::new();
    let mut srv = grpc::server::Server::new();
    srv.set_handler(Handler {});
    let lis_clone = lis.clone();
    tokio::task::spawn(async move {
        srv.serve(&lis_clone).await;
        println!("serve returned for listener 1!");
    });

    println!("Creating channel for {}", lis.target());
    let chan_opts = ChannelOptions::default();
    let chan = grpc::client::Channel::new(
        lis.target().as_str(),
        InsecureChannelCredentials::new(),
        chan_opts,
    );

    let outbound = async_stream::stream! {
        yield Box::new(MyReqMessage("My Request 1".to_string())) as Box<dyn Message>;
        yield Box::new(MyReqMessage("My Request 2".to_string()));
        yield Box::new(MyReqMessage("My Request 3".to_string()));
    };

    let req = Request::new(Box::pin(outbound));
    let res = chan.call("/some/method".to_string(), req).await;
    let mut res = res.into_inner();

    while let Some(resp) = res.next().await {
        println!(
            "CALL RESPONSE: {}",
            (resp.unwrap() as Box<dyn Any>)
                .downcast_ref::<MyResMessage>()
                .unwrap()
                .0,
        );
    }
    lis.close().await;
}
