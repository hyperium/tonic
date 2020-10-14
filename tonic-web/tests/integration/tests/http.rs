#![allow(unused)]
use hyper::http::{header, StatusCode};
use hyper::{Body, Client, Method, Request, Uri};
use integration::{pb::test_server::TestServer, Svc};
use tonic::transport::Server;

const CODE_UNIMPLEMENTED: &str = "12";

mod h1 {
    #[test]
    #[ignore]
    fn todo() {}
}

mod h2 {
    #[test]
    #[ignore]
    fn todo() {}
}
