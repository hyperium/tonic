use std::error::Error;
use std::time::Duration;

use rand::rngs::ThreadRng;
use rand::Rng;
use tokio::time;
use tonic::transport::Channel;
use tonic::Request;

use routeguide::route_guide_client::RouteGuideClient;
use routeguide::{Point, Rectangle, RouteNote};

pub mod routeguide {
    tonic::include_proto!("routeguide");
}

async fn print_features(client: &mut RouteGuideClient<Channel>) -> Result<(), Box<dyn Error>> {
    let rectangle = Rectangle {
        lo: Some(Point {
            latitude: 400_000_000,
            longitude: -750_000_000,
        }),
        hi: Some(Point {
            latitude: 420_000_000,
            longitude: -730_000_000,
        }),
    };

    let mut stream = client
        .list_features(Request::new(rectangle))
        .await?
        .into_inner();

    while let Some(feature) = stream.message().await? {
        println!("NOTE = {:?}", feature);
    }

    Ok(())
}

async fn run_record_route(client: &mut RouteGuideClient<Channel>) -> Result<(), Box<dyn Error>> {
    let mut rng = rand::thread_rng();
    let point_count: i32 = rng.gen_range(2..100);

    let mut points = vec![];
    for _ in 0..=point_count {
        points.push(random_point(&mut rng))
    }

    println!("Traversing {} points", points.len());
    let request = Request::new(tokio_stream::iter(points));

    match client.record_route(request).await {
        Ok(response) => println!("SUMMARY: {:?}", response.into_inner()),
        Err(e) => println!("something went wrong: {:?}", e),
    }

    Ok(())
}

async fn run_route_chat(client: &mut RouteGuideClient<Channel>) -> Result<(), Box<dyn Error>> {
    let start = time::Instant::now();

    let outbound = async_stream::stream! {
        let mut interval = time::interval(Duration::from_secs(1));

        loop {
            let time = interval.tick().await;
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

    let response = client.route_chat(Request::new(outbound)).await?;
    let mut inbound = response.into_inner();

    while let Some(note) = inbound.message().await? {
        println!("NOTE = {:?}", note);
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = RouteGuideClient::connect("http://[::1]:10000").await?;

    println!("*** SIMPLE RPC ***");
    let response = client
        .get_feature(Request::new(Point {
            latitude: 409_146_138,
            longitude: -746_188_906,
        }))
        .await?;
    println!("RESPONSE = {:?}", response);

    println!("\n*** SERVER STREAMING ***");
    print_features(&mut client).await?;

    println!("\n*** CLIENT STREAMING ***");
    run_record_route(&mut client).await?;

    println!("\n*** BIDIRECTIONAL STREAMING ***");
    run_route_chat(&mut client).await?;

    Ok(())
}

fn random_point(rng: &mut ThreadRng) -> Point {
    let latitude = (rng.gen_range(0..180) - 90) * 10_000_000;
    let longitude = (rng.gen_range(0..360) - 180) * 10_000_000;
    Point {
        latitude,
        longitude,
    }
}
