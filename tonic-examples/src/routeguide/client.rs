use futures::TryStreamExt;
use hyper::client::conn::Builder;
use hyper::client::connect::HttpConnector;
use hyper::client::service::{Connect, MakeService};
use route_guide::{Point, RouteNote};
use std::time::{Duration, Instant};
use tokio::timer::Interval;
use tonic::service::add_origin::AddOrigin;
use tonic::Request;

mod route_guide {
    include!(concat!(env!("OUT_DIR"), "/routeguide.rs"));
    tonic::client!(service = "routeguide.RouteGuide", proto = "self");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let origin = http::Uri::from_static("http://[::1]:10000");

    let settings = Builder::new().http2_only(true).clone();
    let mut maker = Connect::new(HttpConnector::new(1), settings);

    let svc = maker.make_service(origin.clone()).await?;
    let svc = AddOrigin::new(svc, origin);

    let mut client = route_guide::RouteGuideClient::new(svc);

    let start = Instant::now();

    let response = client
        .get_feature(Request::new(Point {
            latitude: 409146138,
            longitude: -746188906,
        }))
        .await?;

    println!("FEATURE = {:?}", response);

    let outbound = async_stream::try_stream! {
        let mut interval =  Interval::new_interval(Duration::from_secs(1));

        while let Some(time) = interval.next().await {
            let elapsed = time.duration_since(start);
            let note = RouteNote {
                location: Some(Point {
                    latitude: 409146138 + elapsed.as_secs() as i32,
                    longitude: -746188906,
                }),
                message: format!("at {:?}", elapsed),
            };

            yield note;
        }
    };

    let request = Request::new(outbound);

    let response = client.route_chat(request).await?;

    let mut inbound = response.into_inner();

    while let Some(note) = inbound.try_next().await? {
        println!("NOTE = {:?}", note);
    }

    Ok(())
}
