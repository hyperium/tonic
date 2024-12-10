use tokio_stream::StreamExt;
use tonic_reflection::pb::{
    server_reflection_client::ServerReflectionClient, server_reflection_request::MessageRequest,
    server_reflection_response::MessageResponse, ServerReflectionRequest, ServerReflectionResponse,
};

fn parse_response(resp: ServerReflectionResponse) {
    let message_response = resp.message_response.expect("message response");

    if let MessageResponse::ListServicesResponse(list_response) = message_response {
        for svc in list_response.service {
            println!("\tfound service: `{}`", svc.name);
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = tonic::transport::Endpoint::new("http://[::1]:50052")?
        .connect()
        .await?;

    let mut client = ServerReflectionClient::new(conn);

    let list_services_request = ServerReflectionRequest {
        host: "host".into(),
        message_request: Some(MessageRequest::ListServices("list".into())),
    };

    let request_stream = tokio_stream::once(list_services_request);
    let mut inbound = client
        .server_reflection_info(request_stream)
        .await?
        .into_inner();

    while let Some(recv) = inbound.next().await {
        match recv {
            Ok(resp) => parse_response(resp),
            Err(e) => println!("\tdid not receive response due to error: `{}`", e),
        }
    }

    Ok(())
}
