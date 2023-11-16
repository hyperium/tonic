use arrow_flight::{arrow, client::FlightClient, server::FlightService};
use bencher::{benchmark_group, benchmark_main};
use prost::bytes::Bytes;
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, time};
use tonic::codegen::tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tonic::transport::Server;

#[derive(Default, Debug)]
struct Opts {
    manual_client: bool,
    manual_server: bool,
    request_chunks: usize,
    request_payload: arrow::FlightData,
    response_payload: arrow::FlightData,
}

fn req_64kb_resp_64kb_10_chunks_native_client_native_server(b: &mut bencher::Bencher) {
    opts()
        .request_chunks(10)
        .request_payload(make_payload(64 * 1024))
        .response_payload(make_payload(64 * 1024))
        .bench(b);
}

fn req_64kb_resp_64kb_10_chunks_native_client_manual_server(b: &mut bencher::Bencher) {
    opts()
        .manual_server()
        .request_chunks(10)
        .request_payload(make_payload(64 * 1024))
        .response_payload(make_payload(64 * 1024))
        .bench(b);
}

fn req_64kb_resp_64kb_10_chunks_manual_client_native_server(b: &mut bencher::Bencher) {
    opts()
        .manual_client()
        .request_chunks(10)
        .request_payload(make_payload(64 * 1024))
        .response_payload(make_payload(64 * 1024))
        .bench(b);
}

fn req_64kb_resp_64kb_10_chunks_manual_client_manual_server(b: &mut bencher::Bencher) {
    opts()
        .manual_server()
        .manual_client()
        .request_chunks(10)
        .request_payload(make_payload(64 * 1024))
        .response_payload(make_payload(64 * 1024))
        .bench(b);
}

fn req_1mb_resp_1mb_10_chunks_native_client_native_server(b: &mut bencher::Bencher) {
    opts()
        .request_chunks(10)
        .request_payload(make_payload(1 * 1024 * 1024))
        .response_payload(make_payload(1 * 1024 * 1024))
        .bench(b);
}

fn req_1mb_resp_1mb_10_chunks_native_client_manual_server(b: &mut bencher::Bencher) {
    opts()
        .manual_server()
        .request_chunks(10)
        .request_payload(make_payload(1 * 1024 * 1024))
        .response_payload(make_payload(1 * 1024 * 1024))
        .bench(b);
}

fn req_1mb_resp_1mb_10_chunks_manual_client_native_server(b: &mut bencher::Bencher) {
    opts()
        .manual_client()
        .request_chunks(10)
        .request_payload(make_payload(1 * 1024 * 1024))
        .response_payload(make_payload(1 * 1024 * 1024))
        .bench(b);
}

fn req_1mb_resp_1mb_10_chunks_manual_client_manual_server(b: &mut bencher::Bencher) {
    opts()
        .manual_server()
        .manual_client()
        .request_chunks(10)
        .request_payload(make_payload(1 * 1024 * 1024))
        .response_payload(make_payload(1 * 1024 * 1024))
        .bench(b);
}

fn make_payload(size: usize) -> arrow::FlightData {
    arrow::FlightData {
        flight_descriptor: Some(arrow::FlightDescriptor {
            cmd: Bytes::from("cmd"),
            path: vec!["/path/to/data".to_string()],
            ..Default::default()
        }),
        data_header: Bytes::from("data_header"),
        app_metadata: Bytes::from("app_metadata"),
        data_body: Bytes::from(vec![b'a'; size]),
    }
}

fn opts() -> Opts {
    Opts {
        request_chunks: 1,
        ..Default::default()
    }
}

impl Opts {
    fn manual_client(mut self) -> Self {
        self.manual_client = true;
        self
    }

    fn manual_server(mut self) -> Self {
        self.manual_server = true;
        self
    }

    fn request_chunks(mut self, chunks: usize) -> Self {
        self.request_chunks = chunks;
        self
    }

    fn request_payload(mut self, payload: arrow::FlightData) -> Self {
        self.request_payload = payload;
        self
    }

    fn response_payload(mut self, payload: arrow::FlightData) -> Self {
        self.response_payload = payload;
        self
    }

    fn bench(self, b: &mut bencher::Bencher) {
        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("runtime"),
        );

        b.bytes = ((self.request_payload.data_body.len() + self.response_payload.data_body.len())
            * self.request_chunks) as u64;

        spawn_server(&rt, &self);

        let channel = rt.block_on(async {
            time::sleep(Duration::from_millis(100)).await;
            tonic::transport::Endpoint::from_static("http://127.0.0.1:1500")
                .connect()
                .await
                .unwrap()
        });

        let do_exchange = || async {
            let mut client = if self.manual_client {
                FlightClient::manual(channel.clone())
            } else {
                FlightClient::native(channel.clone())
            };
            let (tx, rx) = mpsc::channel(8192);
            let mut server_stream = client
                .do_exchange(ReceiverStream::new(rx))
                .await
                .unwrap()
                .into_inner();
            for _ in 0..self.request_chunks {
                tx.send(self.response_payload.clone()).await.unwrap();
                server_stream.next().await.unwrap().unwrap();
            }
        };

        b.iter(move || {
            rt.block_on(do_exchange());
        });
    }
}

fn spawn_server(rt: &tokio::runtime::Runtime, opts: &Opts) {
    let addr = "127.0.0.1:1500";

    let response_payload = opts.response_payload.clone();
    let manual_server = opts.manual_server;
    let srv = rt.block_on(async move {
        let flight_service = FlightService::new(response_payload);
        if manual_server {
            Server::builder()
                .add_service(
                    arrow::manual::flight_service_server::FlightServiceServer::new(flight_service),
                )
                .serve(addr.parse().unwrap())
        } else {
            Server::builder()
                .add_service(arrow::flight_service_server::FlightServiceServer::new(
                    flight_service,
                ))
                .serve(addr.parse().unwrap())
        }
    });

    rt.spawn(async move { srv.await.unwrap() });
}

benchmark_group!(
    req_64kb_resp_64kb_10_chunks,
    req_64kb_resp_64kb_10_chunks_native_client_native_server,
    req_64kb_resp_64kb_10_chunks_manual_client_native_server,
    req_64kb_resp_64kb_10_chunks_native_client_manual_server,
    req_64kb_resp_64kb_10_chunks_manual_client_manual_server
);

benchmark_group!(
    req_1mb_resp_1mb_10_chunks,
    req_1mb_resp_1mb_10_chunks_native_client_native_server,
    req_1mb_resp_1mb_10_chunks_manual_client_native_server,
    req_1mb_resp_1mb_10_chunks_native_client_manual_server,
    req_1mb_resp_1mb_10_chunks_manual_client_manual_server
);

benchmark_main!(req_64kb_resp_64kb_10_chunks, req_1mb_resp_1mb_10_chunks);
