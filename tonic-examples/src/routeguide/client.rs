use futures::TryStreamExt;
use route_guide::{Point, RouteNote};
use std::time::{Duration, Instant};
use tokio::timer::Interval;

pub mod route_guide {
    tonic::include_proto!("routeguide");
}

use route_guide::client::RouteGuideClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = RouteGuideClient::connect("http://[::1]:10000")?;

    let start = Instant::now();

    let response = client
        .get_feature(Point {
            latitude: 409146138,
            longitude: -746188906,
        })
        .await?;

    println!("FEATURE = {:?}", response);

    let outbound = async_stream::stream! {
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

    let response = client.route_chat(outbound).await?;

    let mut inbound = response.into_inner();

    while let Some(note) = inbound.try_next().await? {
        println!("NOTE = {:?}", note);
    }

    Ok(())
}
