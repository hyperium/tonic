//! This examples shows how you can combine `hyper-rustls` and `tonic` to
//! provide a custom `ClientConfig` for the tls configuration.

pub mod pb {
    tonic::include_proto!("/grpc.examples.echo");
}

use hyper::{client::HttpConnector, Uri};
use pb::{echo_client::EchoClient, EchoRequest};
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fd = std::fs::File::open("examples/data/tls/ca.pem")?;

    let mut roots = RootCertStore::empty();

    let mut buf = std::io::BufReader::new(&fd);
    let certs = rustls_pemfile::certs(&mut buf)?;
    roots.add_parsable_certificates(&certs);

    let tls = ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let mut http = HttpConnector::new();
    http.enforce_http(false);

    // We have to do some wrapping here to map the request type from
    // `https://example.com` -> `https://[::1]:50051` because `rustls`
    // doesn't accept ip's as `ServerName`.
    let connector = tower::ServiceBuilder::new()
        .layer_fn(move |s| {
            let tls = tls.clone();

            hyper_rustls::HttpsConnectorBuilder::new()
                .with_tls_config(tls)
                .https_or_http()
                .enable_http2()
                .wrap_connector(s)
        })
        // Since our cert is signed with `example.com` but we actually want to connect
        // to a local server we will override the Uri passed from the `HttpsConnector`
        // and map it to the correct `Uri` that will connect us directly to the local server.
        .map_request(|_| Uri::from_static("https://[::1]:50051"))
        .service(http);

    let client = hyper::Client::builder().build(connector);

    // Hyper expects an absolute `Uri` to allow it to know which server to connect too.
    // Currently, tonic's generated code only sets the `path_and_query` section so we
    // are going to write a custom tower layer in front of the hyper client to add the
    // scheme and authority.
    //
    // Again, this Uri is `example.com` because our tls certs is signed with this SNI but above
    // we actually map this back to `[::1]:50051` before the `Uri` is passed to hyper's `HttpConnector`
    // to allow it to correctly establish the tcp connection to the local `tls-server`.
    let uri = Uri::from_static("https://example.com");
    let svc = tower::ServiceBuilder::new()
        .map_request(move |mut req: http::Request<tonic::body::BoxBody>| {
            let uri = Uri::builder()
                .scheme(uri.scheme().unwrap().clone())
                .authority(uri.authority().unwrap().clone())
                .path_and_query(req.uri().path_and_query().unwrap().clone())
                .build()
                .unwrap();

            *req.uri_mut() = uri;
            req
        })
        .service(client);

    let mut client = EchoClient::new(svc);

    let request = tonic::Request::new(EchoRequest {
        message: "hello".into(),
    });

    let response = client.unary_echo(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
