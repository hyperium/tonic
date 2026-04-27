use protobuf::proto;

#[allow(unused)]
mod generated {
    pub mod routeguide {
        include!("generated/generated.rs"); // Contains messages
        include!("generated/route_guide_grpc.pb.rs"); // Contains grpc stubs
    }
}

use std::sync::Arc;

use grpc::client::Channel;
use grpc::client::ChannelOptions;
use grpc::client::Invoke;
use grpc::credentials::LocalChannelCredentials;
use tokio::task;

use crate::generated::routeguide::Feature;
use crate::generated::routeguide::Point;
use crate::generated::routeguide::Rectangle;
use crate::generated::routeguide::RouteNote;
use crate::generated::routeguide::route_guide_client::RouteGuideClient;

fn print_feature(feature: Feature) {
    println!(
        "Response = Name = \"{}\", Point: {{{}, {}}}",
        feature.name(),
        feature.location().latitude(),
        feature.location().longitude()
    );
}

async fn get_feature<T: Invoke>(client: &RouteGuideClient<T>, point: Point) {
    let response = client.get_feature(point).await.expect("RPC failed");
    print_feature(response);
}

async fn list_features<T: Invoke>(client: &RouteGuideClient<T>, rect: Rectangle) {
    let mut response_stream = client.list_features(rect).start().await;
    while let Some(feature) = response_stream.next().await {
        print_feature(feature);
    }
    let status = response_stream.status().await;
    assert_eq!(status.code(), grpc::StatusCode::Ok, "{:?}", status);
}

async fn record_route<T: Invoke>(client: &RouteGuideClient<T>) {
    // Create a random number of random points.
    let point_count = rand::random_range(2..=30); // Traverse at least two points
    let mut points = Vec::with_capacity(point_count);
    for _ in 0..point_count {
        points.push(random_point());
    }
    println!("Traversing {point_count} points.");
    let mut stream = client.record_route().await;
    for point in points {
        stream.send_message(&point).await.expect("RPC failed");
    }
    let response = stream.await.expect("RPC failed");
    println!(
        "Route summary: Point count: {}, Distance: {}",
        response.point_count(),
        response.distance()
    );
}

async fn route_chat<T: Invoke>(client: &RouteGuideClient<T>) {
    let notes = vec![
        proto!(RouteNote {
            location: Point {
                latitude: 0,
                longitude: 1,
            },
            message: format!("Message One"),
        }),
        proto!(RouteNote {
            location: Point {
                latitude: 0,
                longitude: 2,
            },
            message: format!("Message Two"),
        }),
        proto!(RouteNote {
            location: Point {
                latitude: 0,
                longitude: 3,
            },
            message: format!("Message Three"),
        }),
        proto!(RouteNote {
            location: Point {
                latitude: 0,
                longitude: 1,
            },
            message: format!("Message Four"),
        }),
        proto!(RouteNote {
            location: Point {
                latitude: 0,
                longitude: 1,
            },
            message: format!("Message Five"),
        }),
    ];
    let (mut tx, mut rx) = client.route_chat().await;
    let handle = task::spawn(async move {
        while let Some(response) = rx.next().await {
            println!(
                "Got message {} at Point: {{{}, {}}}",
                response.message(),
                response.location().latitude(),
                response.location().longitude()
            );
        }
    });
    for note in notes {
        tx.send_message(note).await.expect("RPC errored");
    }
    // Dropping tx causes the client to send a "half close" to the server.
    drop(tx);
    // Await the async work where we read from the server.
    handle.await.unwrap();
}

fn random_point() -> Point {
    let latitude = (rand::random_range(0..180) - 90) * 10_000_000;
    let longitude = (rand::random_range(0..360) - 180) * 10_000_000;
    proto!(Point {
        latitude,
        longitude
    })
}

#[tokio::main]
async fn main() {
    // Create a new gRPC channel:
    let channel = Channel::new(
        "dns:///localhost:50051",
        Arc::new(LocalChannelCredentials::new()),
        ChannelOptions::default(),
    );
    let client = RouteGuideClient::new(channel);

    println!("*** SIMPLE RPC ***");
    get_feature(
        &client,
        proto!(Point {
            latitude: 409_146_138,
            longitude: -746_188_906
        }),
    )
    .await;

    println!("*** MISSING FEATURE ***");
    get_feature(
        &client,
        proto!(Point {
            latitude: 0,
            longitude: 0
        }),
    )
    .await;

    println!("*** FEATURES IN RANGE ***");
    list_features(
        &client,
        proto!(Rectangle {
            lo: Point {
                latitude: 400000000,
                longitude: -750000000
            },
            hi: Point {
                latitude: 420000000,
                longitude: -730000000
            },
        }),
    )
    .await;

    println!("*** RECORD ROUTE ***");
    record_route(&client).await;

    println!("*** ROUTE CHAT ***");
    route_chat(&client).await;
}
