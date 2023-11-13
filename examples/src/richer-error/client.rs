use tonic_types::StatusExt;

use hello_world::greeter_client::GreeterClient;
use hello_world::HelloRequest;

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = GreeterClient::connect("http://[::1]:50051").await?;

    let request = tonic::Request::new(HelloRequest {
        // Valid request
        // name: "Tonic".into(),
        // Name cannot be empty
        name: "".into(),
        // Name is too long
        // name: "some excessively long name".into(),
    });

    let response = match client.say_hello(request).await {
        Ok(response) => response,
        Err(status) => {
            println!(" Error status received. Extracting error details...\n");

            let err_details = status.get_error_details();

            if let Some(bad_request) = err_details.bad_request() {
                // Handle bad_request details
                println!(" {:?}", bad_request);
            }
            if let Some(help) = err_details.help() {
                // Handle help details
                println!(" {:?}", help);
            }
            if let Some(localized_message) = err_details.localized_message() {
                // Handle localized_message details
                println!(" {:?}", localized_message);
            }

            println!();

            return Ok(());
        }
    };

    println!(" Successful response received.\n\n {:?}\n", response);

    Ok(())
}
