use clap::{arg_enum, values_t, App, Arg};
use tonic_interop::client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init();

    let matches = App::new("My Super Program")
        .version("1.0")
        .about("Does awesome things")
        .arg(
            Arg::with_name("test_case")
                .long("test_case")
                .value_name("TESTCASE")
                .help(
                    "The name of the test case to execute. For example,
                \"empty_unary\".",
                )
                .possible_values(&Testcase::variants())
                .default_value("large_unary")
                .takes_value(true)
                .min_values(1)
                .use_delimiter(true),
        )
        .get_matches();

    let test_cases = values_t!(matches, "test_case", Testcase).unwrap_or_else(|e| e.exit());

    let addr = "127.0.0.1:10000".parse()?;

    let mut client = client::create(addr).await?;

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
            _ => unimplemented!(),
        }

        for result in test_results {
            println!("  {}", result);
        }
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
