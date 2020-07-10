use interop::client;
use std::time::Duration;
use structopt::{clap::arg_enum, StructOpt};
use tonic::transport::Endpoint;
use tonic::transport::{Certificate, ClientTlsConfig};

#[derive(StructOpt)]
struct Opts {
    #[structopt(name = "use_tls", long)]
    use_tls: bool,

    #[structopt(
        long = "test_case",
        use_delimiter = true,
        min_values = 1,
        possible_values = &Testcase::variants()
    )]
    test_case: Vec<Testcase>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    interop::trace_init();

    let matches = Opts::from_args();

    let test_cases = matches.test_case;

    #[allow(unused_mut)]
    let mut endpoint = Endpoint::from_static("http://localhost:10000")
        .timeout(Duration::from_secs(5))
        .concurrency_limit(30);

    if matches.use_tls {
        let pem = tokio::fs::read("interop/data/ca.pem").await?;
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
            Testcase::empty_unary => client::empty_unary(&mut client, &mut test_results).await,
            Testcase::large_unary => client::large_unary(&mut client, &mut test_results).await,
            Testcase::client_streaming => {
                client::client_streaming(&mut client, &mut test_results).await
            }
            Testcase::server_streaming => {
                client::server_streaming(&mut client, &mut test_results).await
            }
            Testcase::ping_pong => client::ping_pong(&mut client, &mut test_results).await,
            Testcase::empty_stream => client::empty_stream(&mut client, &mut test_results).await,
            Testcase::status_code_and_message => {
                client::status_code_and_message(&mut client, &mut test_results).await
            }
            Testcase::special_status_message => {
                client::special_status_message(&mut client, &mut test_results).await
            }
            Testcase::unimplemented_method => {
                client::unimplemented_method(&mut client, &mut test_results).await
            }
            Testcase::unimplemented_service => {
                client::unimplemented_service(&mut unimplemented_client, &mut test_results).await
            }
            Testcase::custom_metadata => {
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

arg_enum! {
    #[derive(Debug, Copy, Clone)]
    #[allow(non_camel_case_types)]
    enum Testcase {
        empty_unary,
        cacheable_unary,
        large_unary,
        client_compressed_unary,
        server_compressed_unary,
        client_streaming,
        client_compressed_streaming,
        server_streaming,
        server_compressed_streaming,
        ping_pong,
        empty_stream,
        compute_engine_creds,
        jwt_token_creds,
        oauth2_auth_token,
        per_rpc_creds,
        custom_metadata,
        status_code_and_message,
        special_status_message,
        unimplemented_method,
        unimplemented_service,
        cancel_after_begin,
        cancel_after_first_response,
        timeout_on_sleeping_server,
        concurrent_large_unary
    }
}
