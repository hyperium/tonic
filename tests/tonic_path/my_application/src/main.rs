mod pb {
    include!(concat!(env!("OUT_DIR"), "/greeter.rs"));
}

#[derive(Default)]
struct MyGreeter;

#[wrapper::tonic::async_trait]
impl pb::greeter_server::Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: wrapper::tonic::Request<pb::HelloRequest>,
    ) -> Result<wrapper::tonic::Response<pb::HelloResponse>, wrapper::tonic::Status> {
        let name = request.into_inner().name;
        Ok(wrapper::tonic::Response::new(pb::HelloResponse {
            message: format!("Hello, {name}!"),
        }))
    }
}

fn main() {
    let _server = pb::greeter_server::GreeterServer::new(MyGreeter);
}

#[cfg(test)]
#[test]
fn test_generated_types_resolve_through_wrapper() {
    // Constructing both the server and client types proves the generated
    // code resolved `wrapper::tonic::*` and `wrapper::tonic_prost::ProstCodec`
    // without any direct `tonic` dependency in this crate's manifest.
    let _server = pb::greeter_server::GreeterServer::new(MyGreeter);
    fn _assert_client_type<T>() {}
    _assert_client_type::<pb::greeter_client::GreeterClient<wrapper::tonic::transport::Channel>>();
}
