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

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use grpc::client::Channel;
use grpc::credentials::ChannelCredentials;
use grpc::credentials::LocalChannelCredentials;
use grpc_rustls::RootCertificates;
use grpc_rustls::StaticRootCertificatesProvider;
use grpc_rustls::client::ClientTlsConfig;
use grpc_rustls::client::RustlsChannelCredentials;
use hdrhistogram::Histogram;
use protobuf::proto;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tonic::Status;

use crate::generated::grpc::testing::Payload;
use crate::generated::grpc::testing::PayloadType;
use crate::generated::grpc::testing::SimpleRequest;
use crate::generated::grpc::testing::SimpleResponse;
use crate::generated::grpc::testing::benchmark_service_client::BenchmarkServiceClient;
use crate::generated::services::grpc::testing::ClientConfig;
use crate::generated::services::grpc::testing::ClientStats;
use crate::generated::services::grpc::testing::ClientType;
use crate::generated::services::grpc::testing::HistogramData;
use crate::generated::services::grpc::testing::HistogramParams;
use crate::generated::services::grpc::testing::RpcType;
use crate::generated::services::grpc::testing::load_params::Load;
use crate::generated::services::grpc::testing::payload_config::Payload::BytebufParams;
use crate::generated::services::grpc::testing::payload_config::Payload::SimpleParams;
use crate::rusage::Rusage;

const CA_PEM: &[u8] = include_bytes!("../data/tls/ca.pem");

pub struct BenchmarkClient {
    histogram_params: HistogramParams,
    histogram_count: usize,
    last_reset_time: Instant,
    last_rusage: Rusage,
    cancellation_requested: Arc<AtomicBool>,
    stats_response_rx: mpsc::Receiver<Histogram<u64>>,
    stats_request_tx: watch::Sender<StatsRequestStatus>,
}

impl BenchmarkClient {
    pub fn start(config: ClientConfig) -> Result<BenchmarkClient, Status> {
        println!("{:?}", config);

        // Parse and validate the config.
        match config.client_type() {
            ClientType::SyncClient | ClientType::AsyncClient => (),
            _ => return Err(Status::invalid_argument("Invalid client_type")),
        };

        let payload_type = config
            .payload_config
            .ok_or(Status::invalid_argument("payload_config missing"))?
            .payload
            .ok_or(Status::invalid_argument("payload missing"))?;

        let (payload_req_size, payload_resp_size) = match payload_type {
            BytebufParams(_) => return Err(Status::unimplemented("bytebuf codec not implemented")),
            SimpleParams(params) => (params.req_size as usize, params.resp_size as usize),
            _ => {
                return Err(Status::invalid_argument(format!(
                    "unknown payload type: {:?}",
                    payload_type
                )));
            }
        };

        let load = config
            .load_params
            .ok_or(Status::invalid_argument("load_params missing"))?
            .load
            .ok_or(Status::invalid_argument("load missing"))?;

        // If set, perform an open loop, if not perform a closed loop. An open
        // loop asynchronously starts RPCs based on random start times derived
        // from a Poisson distribution. A closed loop performs RPCs in a
        // blocking manner, and runs the next RPC after the previous RPC
        // completes and returns.
        match load {
            Load::ClosedLoop(_) => {}
            Load::Poisson(_) => {
                // TODO: Implement poisson load distribution when adding support
                // for xDS benchmarks.
                return Err(Status::unimplemented(
                    "Poisson load generation not supported",
                ));
            }
        };

        if config.client_channels <= 0 {
            return Err(Status::invalid_argument(
                "client_channels must be greater than 0",
            ));
        }

        if config.outstanding_rpcs_per_channel <= 0 {
            return Err(Status::invalid_argument(
                "outstanding_rpcs_per_channel must be greater than 0",
            ));
        }

        let channel_count = config.client_channels as usize;
        let histogram_params = config
            .histogram_params
            .ok_or(Status::invalid_argument("missing histogram_params"))?;

        // Check and set security options.
        let mut authority: Option<String> = None;
        let credentials = if let Some(params) = &config.security_params {
            let mut tls_config = ClientTlsConfig::new();
            if params.use_test_ca {
                tls_config = tls_config.with_root_certificates_provider(
                    StaticRootCertificatesProvider::new(RootCertificates::from_pem(CA_PEM)),
                );
            };
            if !params.server_host_override.is_empty() {
                authority = Some(params.server_host_override.clone());
            }
            let tls_creds = RustlsChannelCredentials::new(tls_config).map_err(|err| {
                Status::internal(format!("failed to create TLS credentials: {err}"))
            })?;
            Arc::new(tls_creds) as Arc<dyn ChannelCredentials>
        } else {
            LocalChannelCredentials::new_arc()
        };

        let rpc_count_per_conn = config.outstanding_rpcs_per_channel as usize;

        let rpc_type = match config.rpc_type() {
            RpcType::Unary => RPCType::Unary,
            RpcType::Streaming => RPCType::Streaming,
            _ => return Err(Status::invalid_argument("invalid rpc_type")),
        };

        let cancellation_requested = Arc::new(AtomicBool::new(false));

        if config.server_targets.is_empty() {
            return Err(Status::invalid_argument("server_targets cannot be empty"));
        }
        let mut server_targets = config
            .server_targets
            .iter()
            .map(|s| format!("dns:///{s}"))
            .cycle();
        let num_tasks = channel_count * rpc_count_per_conn;
        let (stats_response_tx, stats_response_rx) = mpsc::channel(num_tasks);
        let (stats_request_tx, stats_request_rx) = watch::channel(StatsRequestStatus::Waiting);
        let histogram =
            Histogram::new_with_max(histogram_params.max_possible as u64, 3).map_err(|err| {
                Status::invalid_argument(format!(
                    "failed to build histogram with given max_possible value: {}",
                    err
                ))
            })?;

        for _ in 0..channel_count {
            let target = server_targets.next().unwrap(); // cyclic, non-empty iterator.
            let mut builder = Channel::builder(target, credentials.clone());
            if let Some(authority) = &authority {
                builder = builder.authority(authority);
            }
            let channel = builder.build();

            let args = TestOptions {
                rpc_opts: RpcOptions {
                    payload_req_size,
                    payload_resp_size,
                    client: BenchmarkServiceClient::new(channel),
                    histogram: histogram.clone(),
                    stats_response_tx: stats_response_tx.clone(),
                    stats_request_rx: stats_request_rx.clone(),
                    cancellation_requested: cancellation_requested.clone(),
                },
                rpc_count_per_conn,
                rpc_type,
            };
            start_rpcs(args);
        }

        Ok(BenchmarkClient {
            histogram_params,
            histogram_count: num_tasks,
            last_reset_time: Instant::now(),
            cancellation_requested,
            last_rusage: Rusage::now().map_err(|err| {
                Status::internal(format!("failed to query system resource usage: {err}"))
            })?,
            stats_response_rx,
            stats_request_tx,
        })
    }

    pub async fn get_stats(&mut self, reset: bool) -> Result<ClientStats, Status> {
        let mut aggregated = Histogram::new_with_max(self.histogram_params.max_possible as u64, 3)
            .map_err(|err| Status::internal(format!("failed to configure histogram: {err}")))?;

        // Signal tasks to report tasks.
        let req = StatsRequestStatus::Requested(reset);
        self.stats_request_tx
            .send(req)
            .map_err(|_| Status::internal("client tasks exited unexpectedly"))?;

        // Wait for all the histograms.
        for _ in 0..self.histogram_count {
            let histogram =
                tokio::time::timeout(Duration::from_secs(2), self.stats_response_rx.recv())
                    .await
                    .map_err(|_| Status::deadline_exceeded("timeout waiting for stats"))?
                    .ok_or(Status::internal("client tasks exited unexpectedly"))?;

            aggregated.add(histogram).map_err(|err| {
                Status::internal(format!("error while merging histograms: {}", err))
            })?;
        }

        let now = Instant::now();
        let wall_time_elapsed = now.duration_since(self.last_reset_time);
        let latest_rusage = Rusage::now().map_err(|err| {
            Status::internal(format!("failed to query system resource usage: {err}"))
        })?;

        let user_time_ns = latest_rusage.user_time_nanos() - self.last_rusage.user_time_nanos();
        let system_time_ns =
            latest_rusage.system_time_nanos() - self.last_rusage.system_time_nanos();

        if reset {
            self.last_rusage = latest_rusage;
            self.last_reset_time = now;
        }
        let resolution = 1_f64 + self.histogram_params.resolution.max(0.01_f64);
        let mut base = 1_f64;
        // Calculating the mean and stddev involves iterating over the
        // histogram, so save the values.
        let mean = aggregated.mean();
        let stddev = aggregated.stdev();
        let variance = stddev * stddev;
        let mut histogram_data = HistogramData {
            bucket: Vec::new(),
            min_seen: aggregated.min() as f64,
            max_seen: aggregated.max() as f64,
            sum: mean * aggregated.len() as f64,
            sum_of_squares: variance * aggregated.len() as f64
                + aggregated.len() as f64 * mean * mean,
            count: aggregated.len() as f64,
        };

        for freq in aggregated.iter_log(1, resolution).skip(1) {
            histogram_data
                .bucket
                .push(freq.count_since_last_iteration() as u32);
            base *= resolution;
        }

        // The driver expects values for all buckets in the range, not just the
        // range of buckets that have values.
        while base < self.histogram_params.max_possible {
            histogram_data.bucket.push(0);
            base *= resolution;
        }

        Ok(ClientStats {
            latencies: Some(histogram_data),
            time_elapsed: wall_time_elapsed.as_nanos() as f64 / 1e9,
            time_user: user_time_ns as f64 / 1e9,
            time_system: system_time_ns as f64 / 1e9,
            // The following fields are not set by Java and Go.
            request_results: Vec::new(),
            cq_poll_count: 0,
            core_stats: None,
        })
    }
}

impl Drop for BenchmarkClient {
    fn drop(&mut self) {
        self.cancellation_requested.store(true, Ordering::Relaxed);
    }
}

struct TestOptions {
    rpc_count_per_conn: usize,
    rpc_type: RPCType,
    rpc_opts: RpcOptions,
}

#[derive(Clone)]
struct RpcOptions {
    payload_req_size: usize,
    payload_resp_size: usize,
    client: BenchmarkServiceClient<Channel>,
    stats_request_rx: watch::Receiver<StatsRequestStatus>,
    histogram: Histogram<u64>,
    stats_response_tx: mpsc::Sender<Histogram<u64>>,
    cancellation_requested: Arc<AtomicBool>,
}

fn start_rpcs(test_opts: TestOptions) {
    for _ in 0..test_opts.rpc_count_per_conn {
        let rpc_opts = test_opts.rpc_opts.clone();
        match test_opts.rpc_type {
            RPCType::Streaming => tokio::spawn(blocking_streaming(rpc_opts)),
            RPCType::Unary => tokio::spawn(blocking_unary(rpc_opts)),
        };
    }
}

async fn blocking_unary(mut opts: RpcOptions) {
    let req = proto!(SimpleRequest {
        response_type: PayloadType::Compressable,
        response_size: opts.payload_resp_size as i32,
        payload: Payload {
            r#type: PayloadType::Compressable,
            body: vec![0; opts.payload_req_size],
        },
    });
    let client = opts.client;
    let mut histogram = opts.histogram;

    loop {
        if opts.cancellation_requested.load(Ordering::Relaxed) {
            return;
        }
        if opts
            .stats_request_rx
            .has_changed()
            .is_ok_and(|changed| changed)
        {
            let req_type = *opts.stats_request_rx.borrow_and_update();
            if let StatsRequestStatus::Requested(reset) = req_type {
                let res = opts.stats_response_tx.send(histogram.clone()).await;
                if reset {
                    histogram.reset();
                }
                if res.is_err() {
                    // Client dropped, cancel the task.
                    return;
                }
            }
        }

        let start = Instant::now();
        let res = client.unary_call(req.as_view()).await;
        if res.is_err() {
            continue;
        }
        let elapsed = Instant::now().duration_since(start);
        if let Err(e) = histogram.record(elapsed.as_nanos() as u64) {
            eprintln!("Recorded value greater than configured maximum: {e}");
        }
    }
}

async fn blocking_streaming(mut opts: RpcOptions) {
    let req = proto!(SimpleRequest {
        response_type: PayloadType::Compressable,
        response_size: opts.payload_resp_size as i32,
        payload: Payload {
            r#type: PayloadType::Compressable,
            body: vec![0; opts.payload_req_size],
        },
    });
    let mut histogram = opts.histogram;
    let client = opts.client;
    let (mut tx, mut rx) = client.streaming_call().await;
    let mut resp = SimpleResponse::default();

    loop {
        if opts.cancellation_requested.load(Ordering::Relaxed) {
            return;
        }
        if opts
            .stats_request_rx
            .has_changed()
            .is_ok_and(|changed| changed)
        {
            let req_type = *opts.stats_request_rx.borrow_and_update();
            if let StatsRequestStatus::Requested(reset) = req_type {
                let res = opts.stats_response_tx.send(histogram.clone()).await;
                if reset {
                    histogram.reset();
                }
                if res.is_err() {
                    // Client dropped, cancel the task.
                    return;
                }
            }
        }
        let start = Instant::now();
        // Perform a single ping-pong.
        if tx.send(&req).await.is_err() {
            println!("Stream closed with status: {:?}", rx.status().await);
            return;
        }

        if rx.recv_into(&mut resp).await.is_err() {
            println!("Stream failed with status: {:?}", rx.status().await);
            return;
        };
        let elapsed = Instant::now().duration_since(start);
        if let Err(e) = histogram.record(elapsed.as_nanos() as u64) {
            eprintln!("Recorded value greater than configured maximum: {e}");
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum RPCType {
    Streaming,
    Unary,
}

#[derive(Clone, Copy)]
enum StatsRequestStatus {
    /// Waiting for the first stats request.
    Waiting,
    /// Stats requested. The boolean indicates if accumulated stats should be
    /// dropped after responding.
    Requested(bool),
}
