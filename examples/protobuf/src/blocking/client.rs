use tokio::runtime::{Builder, Runtime};

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

use hello_world::{greeter_client::GreeterClient, HelloReply, HelloRequest};

type StdError = Box<dyn std::error::Error + Send + Sync + 'static>;
type Result<T, E = StdError> = ::std::result::Result<T, E>;

// The order of the fields in this struct is important. The runtime must be the first field and the
// client must be the last field so that when `BlockingClient` is dropped the client is dropped
// before the runtime. Not doing this will result in a deadlock when dropped.
struct BlockingClient {
    rt: Runtime,
    client: GreeterClient<tonic::transport::Channel>,
}

impl BlockingClient {
    pub fn connect<D>(dst: D) -> Result<Self, tonic::transport::Error>
    where
        D: std::convert::TryInto<tonic::transport::Endpoint>,
        D::Error: Into<StdError>,
    {
        let mut rt = Builder::new()
            .basic_scheduler()
            .enable_all()
            .build()
            .unwrap();
        let client = rt.block_on(GreeterClient::connect(dst))?;

        Ok(Self { rt, client })
    }

    pub fn say_hello(
        &mut self,
        request: impl tonic::IntoRequest<HelloRequest>,
    ) -> Result<tonic::Response<HelloReply>, tonic::Status> {
        self.rt.block_on(self.client.say_hello(request))
    }
}

fn main() -> Result<()> {
    let mut client = BlockingClient::connect("http://[::1]:50051")?;

    let request = tonic::Request::new(HelloRequest {
        name: "Tonic".into(),
    });

    let response = client.say_hello(request)?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
