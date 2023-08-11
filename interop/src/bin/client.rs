use interop::client;
use std::{str::FromStr, time::Duration};
use tonic::transport::Endpoint;
use tonic::transport::{Certificate, ClientTlsConfig};

#[derive(Debug)]
struct Opts {
    use_tls: bool,
    test_case: Vec<Testcase>,
}

impl Opts {
    fn parse() -> Result<Self, pico_args::Error> {
        let mut pargs = pico_args::Arguments::from_env();
        Ok(Self {
            use_tls: pargs.contains("--use_tls"),
            test_case: pargs.value_from_fn("--test_case", |test_case| {
                test_case.split(',').map(Testcase::from_str).collect()
            })?,
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    interop::trace_init();

    let matches = Opts::parse()?;

    let test_cases = matches.test_case;

    let scheme = if matches.use_tls { "https" } else { "http" };

    #[allow(unused_mut)]
    let mut endpoint = Endpoint::try_from(format!("{}://localhost:10000", scheme))?
        .timeout(Duration::from_secs(5))
        .concurrency_limit(30);

    if matches.use_tls {
        let pem = std::fs::read_to_string("interop/data/ca.pem")?;
        let ca = Certificate::from_pem(pem);
        endpoint = endpoint.tls_config(
            ClientTlsConfig::new()
                .ca_certificate(ca)
                .domain_name("foo.test.google.fr"),
        )?;
    }

    let channel = endpoint.connect().await?;

    let mut client = client::TestClient::new(channel.clone());
    let mut unimplemented_client = client::UnimplementedClient::new(channel);

    let mut failures = Vec::new();

    for test_case in test_cases {
        println!("{:?}:", test_case);
        let mut test_results = Vec::new();

        match test_case {
            Testcase::EmptyUnary => client::empty_unary(&mut client, &mut test_results).await,
            Testcase::LargeUnary => client::large_unary(&mut client, &mut test_results).await,
            Testcase::ClientStreaming => {
                client::client_streaming(&mut client, &mut test_results).await
            }
            Testcase::ServerStreaming => {
                client::server_streaming(&mut client, &mut test_results).await
            }
            Testcase::PingPong => client::ping_pong(&mut client, &mut test_results).await,
            Testcase::EmptyStream => client::empty_stream(&mut client, &mut test_results).await,
            Testcase::StatusCodeAndMessage => {
                client::status_code_and_message(&mut client, &mut test_results).await
            }
            Testcase::SpecialStatusMessage => {
                client::special_status_message(&mut client, &mut test_results).await
            }
            Testcase::UnimplementedMethod => {
                client::unimplemented_method(&mut client, &mut test_results).await
            }
            Testcase::UnimplementedService => {
                client::unimplemented_service(&mut unimplemented_client, &mut test_results).await
            }
            Testcase::CustomMetadata => {
                client::custom_metadata(&mut client, &mut test_results).await
            }
            _ => unimplemented!(),
        }

        for result in test_results {
            println!("  {}", result);

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
