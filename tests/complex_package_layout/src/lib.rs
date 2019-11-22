mod pb {
    tonic::include_proto!("helloworld");
}

// Ensure that multiple services defined for a single package, spread across multiple `.proto`
// files, are all available in the generated Rust module.
type _Test1 = dyn pb::server::Greeting;
type _Test2 = dyn pb::server::Farewell;
