use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};

pub mod hello_world {
    tonic::include_proto!("helloworld");
}

use hello_world::{
    HelloRequest,
};

//Assess the impact of Rnd
pub fn just_random(string_size: usize) -> Result<(), Box<dyn std::error::Error>> {
    let _rand_name: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(string_size)
        .collect();
    Ok(())
}

//Only testing the load of the compiled GRPC
pub fn load(string_size: usize) -> Result<(), Box<dyn std::error::Error>> {
    let _rand_name: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(string_size)
        .collect();

    //One element POC / HelloRequest
    let _request = tonic::Request::new(HelloRequest {
    name: _rand_name.into(),
    });

    Ok(())
}


//Build out more complex benchmarks with alt protobufs
