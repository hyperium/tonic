use futures::TryStreamExt;
use route_guide::{Point, Rectangle, RouteNote};
use std::error::Error;
use std::time::{Duration, Instant};
use tokio::timer::Interval;
use tonic::transport::Channel;
use tonic::Request;

pub mod route_guide {
    tonic::include_proto!("routeguide");
}

use route_guide::client::RouteGuideClient;

async fn print_feature(
    point: Point,
    client: &mut RouteGuideClient<Channel>,
) -> Result<(), Box<dyn Error>> {
    let response = client.get_feature(Request::new(point)).await?;
    println!("FEATURE = {:?}", response);

    Ok(())
}

async fn print_features(
    rect: Rectangle,
    client: &mut RouteGuideClient<Channel>,
) -> Result<(), Box<dyn Error>> {
    let mut stream = client.list_features(Request::new(rect)).await?.into_inner();

    while let Some(feature) = stream.try_next().await? {
        println!("NOTE = {:?}", feature);
    }

    Ok(())
}

async fn route_chat(client: &mut RouteGuideClient<Channel>) -> Result<(), Box<dyn Error>> {
    let start = Instant::now();

    let outbound = async_stream::try_stream! {
        let mut interval = Interval::new_interval(Duration::from_secs(1));

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = RouteGuideClient::connect("http://[::1]:10000")?;

    let point = Point {
        latitude: 409146138,
        longitude: -746188906,
    };

    print_feature(point, &mut client).await?;

    let rectangle = Rectangle {
        lo: Some(Point {
            latitude: 400000000,
            longitude: -750000000,
        }),
        hi: Some(Point {
            latitude: 420000000,
            longitude: -730000000,
        }),
    };

    print_features(rectangle, &mut client).await?;

    route_chat(&mut client).await?;

    Ok(())
}
