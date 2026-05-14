use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use grpc::client::ChannelOptions;
use grpc::credentials::LocalChannelCredentials;
use grpc::credentials::rustls::RootCertificates;
use grpc::credentials::rustls::StaticProvider;
use grpc::credentials::rustls::client::ClientTlsConfig as GrpcClientTlsConfig;
use grpc::credentials::rustls::client::RustlsChannelCredendials;
use interop::client::InteropTest;
use interop::client::InteropTestUnimplemented;
use interop::client_prost;
use interop::client_protobuf;
use tonic::transport::Certificate;
use tonic::transport::ClientTlsConfig;
use tonic::transport::Endpoint;

#[allow(dead_code)]
#[derive(Debug)]
struct Opts {
    use_tls: bool,
    test_case: Vec<Testcase>,
    codec: Codec,
    server_host: String,
    server_port: u16,
    server_host_override: Option<String>,
    use_test_ca: bool,
    default_service_account: Option<String>,
    oauth_scope: Option<String>,
    service_account_key_file: Option<String>,
    service_config_json: Option<String>,
    additional_metadata: Option<String>,
    google_c2p_universe_domain: Option<String>,
    soak_iterations: usize,
    soak_max_failures: usize,
    soak_per_iteration_max_acceptable_latency_ms: u32,
    soak_overall_timeout_seconds: Option<u32>,
    soak_min_time_ms_between_rpcs: u32,
    soak_num_threads: usize,
}

#[derive(Debug)]
enum Codec {
    Prost,
    Protobuf,
}

impl FromStr for Codec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "prost" => Ok(Codec::Prost),
            "protobuf" => Ok(Codec::Protobuf),
            _ => Err(format!("Invalid codec: {}", s)),
        }
    }
}

impl Opts {
    fn parse() -> Result<Self, pico_args::Error> {
        let mut pargs = pico_args::Arguments::from_env();
        Ok(Self {
            use_tls: pargs.contains("--use_tls"),
            test_case: pargs.value_from_fn("--test_case", |test_case| {
                test_case.split(',').map(Testcase::from_str).collect()
            })?,
            codec: pargs.value_from_str("--codec")?,
            server_host: pargs
                .opt_value_from_str("--server_host")?
                .unwrap_or_else(|| "localhost".to_string()),
            server_port: pargs.opt_value_from_str("--server_port")?.unwrap_or(10000),
            server_host_override: pargs.opt_value_from_str("--server_host_override")?,
            use_test_ca: match pargs.opt_value_from_str::<_, bool>("--use_test_ca") {
                Ok(Some(val)) => val,
                Ok(None) => true,
                Err(_) => true,
            },
            default_service_account: pargs.opt_value_from_str("--default_service_account")?,
            oauth_scope: pargs.opt_value_from_str("--oauth_scope")?,
            service_account_key_file: pargs.opt_value_from_str("--service_account_key_file")?,
            service_config_json: pargs.opt_value_from_str("--service_config_json")?,
            additional_metadata: pargs.opt_value_from_str("--additional_metadata")?,
            google_c2p_universe_domain: pargs.opt_value_from_str("--google_c2p_universe_domain")?,
            soak_iterations: pargs.opt_value_from_str("--soak_iterations")?.unwrap_or(10),
            soak_max_failures: pargs
                .opt_value_from_str("--soak_max_failures")?
                .unwrap_or(0),
            soak_per_iteration_max_acceptable_latency_ms: pargs
                .opt_value_from_str("--soak_per_iteration_max_acceptable_latency_ms")?
                .unwrap_or(1000),
            soak_overall_timeout_seconds: pargs
                .opt_value_from_str("--soak_overall_timeout_seconds")?,
            soak_min_time_ms_between_rpcs: pargs
                .opt_value_from_str("--soak_min_time_ms_between_rpcs")?
                .unwrap_or(0),
            soak_num_threads: pargs.opt_value_from_str("--soak_num_threads")?.unwrap_or(1),
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    interop::trace_init();

    let matches = Opts::parse()?;

    let test_cases = matches.test_case;

    let additional_metadata = if let Some(ref am) = matches.additional_metadata {
        let mut map = tonic::metadata::MetadataMap::new();
        for pair in am.split(';') {
            if pair.is_empty() {
                continue;
            }
            if let Some(colon_idx) = pair.find(':') {
                let (key_str, val_str) = pair.split_at(colon_idx);
                let val_str = &val_str[1..]; // strip the leading colon
                let key_str = key_str.trim();
                let val_str = val_str.trim();

                if key_str.ends_with("-bin") {
                    use base64::Engine;
                    let decoded_val = base64::engine::general_purpose::STANDARD.decode(val_str)?;
                    let key = tonic::metadata::BinaryMetadataKey::from_str(key_str)?;
                    let value = tonic::metadata::MetadataValue::from_bytes(&decoded_val);
                    map.insert_bin(key, value);
                } else {
                    let key = tonic::metadata::AsciiMetadataKey::from_str(key_str)?;
                    let value = tonic::metadata::MetadataValue::try_from(val_str)?;
                    map.insert(key, value);
                }
            }
        }
        Some(map)
    } else {
        None
    };

    let (mut client, mut unimplemented_client): (
        Box<dyn InteropTest>,
        Box<dyn InteropTestUnimplemented>,
    ) = match matches.codec {
        Codec::Prost => {
            let scheme = if matches.use_tls { "https" } else { "http" };
            let host = &matches.server_host;
            let port = matches.server_port;
            let mut endpoint = Endpoint::try_from(format!("{scheme}://{host}:{port}"))?
                .timeout(Duration::from_secs(5))
                .concurrency_limit(30);

            if matches.use_tls {
                let mut tls_config = ClientTlsConfig::new();
                if matches.use_test_ca {
                    let pem = std::fs::read_to_string("interop/data/ca.pem")?;
                    let ca = Certificate::from_pem(pem);
                    tls_config = tls_config.ca_certificate(ca);
                }
                let domain_name = matches
                    .server_host_override
                    .as_deref()
                    .unwrap_or("foo.test.google.fr");
                tls_config = tls_config.domain_name(domain_name);
                endpoint = endpoint.tls_config(tls_config)?;
            }

            let channel = endpoint.connect().await?;

            let interceptor = interop::client::MetadataInterceptor {
                metadata: additional_metadata.unwrap_or_default(),
            };
            (
                Box::new(client_prost::TestClient::new(
                    tonic::codegen::InterceptedService::new(channel.clone(), interceptor.clone()),
                )),
                Box::new(client_prost::UnimplementedClient::new(
                    tonic::codegen::InterceptedService::new(channel, interceptor),
                )),
            )
        }
        Codec::Protobuf => {
            let host = &matches.server_host;
            let port = matches.server_port;
            let target_uri = format!("dns:///{host}:{port}");

            let channel = if matches.use_tls {
                let _ = rustls::crypto::ring::default_provider().install_default();

                let mut tls_config = GrpcClientTlsConfig::new();
                if matches.use_test_ca {
                    let pem = std::fs::read_to_string("interop/data/ca.pem")?;
                    let root_certs = RootCertificates::from_pem(pem);
                    tls_config =
                        tls_config.with_root_certificates_provider(StaticProvider::new(root_certs));
                }
                let creds = RustlsChannelCredendials::new(tls_config)?;
                let domain_name = matches
                    .server_host_override
                    .as_deref()
                    .unwrap_or("test.test.google.fr");
                let channel_options = ChannelOptions::default().override_authority(domain_name);
                grpc::client::Channel::new(&target_uri, Arc::new(creds), channel_options)
            } else {
                grpc::client::Channel::new(
                    &target_uri,
                    Arc::new(LocalChannelCredentials::new()),
                    ChannelOptions::default(),
                )
            };

            (
                Box::new(client_protobuf::TestClient::new(channel.clone())),
                Box::new(client_protobuf::UnimplementedClient::new(channel)),
            )
        }
    };

    let mut failures = Vec::new();

    for test_case in test_cases {
        println!("{test_case:?}:");
        let mut test_results = Vec::new();

        match test_case {
            Testcase::EmptyUnary => client.empty_unary(&mut test_results).await,
            Testcase::CacheableUnary => client.cacheable_unary(&mut test_results).await,
            Testcase::ClientCompressedUnary => {
                client.client_compressed_unary(&mut test_results).await
            }
            Testcase::ServerCompressedUnary => {
                client.server_compressed_unary(&mut test_results).await
            }
            Testcase::LargeUnary => client.large_unary(&mut test_results).await,
            Testcase::ClientStreaming => client.client_streaming(&mut test_results).await,
            Testcase::ServerStreaming => client.server_streaming(&mut test_results).await,
            Testcase::PingPong => client.ping_pong(&mut test_results).await,
            Testcase::EmptyStream => client.empty_stream(&mut test_results).await,
            Testcase::StatusCodeAndMessage => {
                client.status_code_and_message(&mut test_results).await
            }
            Testcase::SpecialStatusMessage => {
                client.special_status_message(&mut test_results).await
            }
            Testcase::UnimplementedMethod => client.unimplemented_method(&mut test_results).await,
            Testcase::UnimplementedService => {
                unimplemented_client
                    .unimplemented_service(&mut test_results)
                    .await
            }
            Testcase::CustomMetadata => client.custom_metadata(&mut test_results).await,
            Testcase::CancelAfterBegin => client.cancel_after_begin(&mut test_results).await,
            Testcase::CancelAfterFirstResponse => {
                client.cancel_after_first_response(&mut test_results).await
            }
            Testcase::TimeoutOnSleepingServer => {
                client.timeout_on_sleeping_server(&mut test_results).await
            }
            _ => unimplemented!(),
        }

        for result in test_results {
            println!("  {result}");

            if result.is_failed() {
                failures.push(result);
            }
        }
    }

    if !failures.is_empty() {
        println!("{} tests failed", failures.len());
        std::process::exit(1);
    }

    Ok(())
}

#[derive(Debug, strum::EnumString)]
#[strum(serialize_all = "snake_case")]
enum Testcase {
    EmptyUnary,
    CacheableUnary,
    LargeUnary,
    ClientCompressedUnary,
    ServerCompressedUnary,
    ClientStreaming,
    ClientCompressedStreaming,
    ServerStreaming,
    ServerCompressedStreaming,
    PingPong,
    EmptyStream,
    ComputeEngineCreds,
    JwtTokenCreds,
    Oauth2AuthToken,
    PerRpcCreds,
    CustomMetadata,
    StatusCodeAndMessage,
    SpecialStatusMessage,
    UnimplementedMethod,
    UnimplementedService,
    CancelAfterBegin,
    CancelAfterFirstResponse,
    TimeoutOnSleepingServer,
    ConcurrentLargeUnary,
}
