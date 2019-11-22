pub mod pb {
    tonic::include_proto!("includer");
    tonic::include_proto!("includee");
}

// Ensure that an RPC service, defined before including a file that defines
// another service in a different protocol buffer package, is not incorrectly
// cleared from the context of its package.
type _Test = dyn pb::server::TopService;
