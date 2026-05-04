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
    // Perform the RPC.
    let response = client.get_feature(point).await.expect("RPC failed");

    // Print its response.
    print_feature(response);
}

async fn list_features<T: Invoke>(client: &RouteGuideClient<T>, rect: Rectangle) {
    // Start the RPC.
    let mut response_stream = client.list_features(rect).await;

    // Receive the response messages.
    while let Some(feature) = response_stream.recv().await {
        print_feature(feature);
    }

    // Confirm the status.
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

    // Start the RPC.
    let mut stream = client.record_route().await;

    // Send the request messages.
    for point in points {
        stream.send(&point).await.expect("RPC failed");
    }

    // Receive the response or status.
    let response = stream.close_and_recv().await.expect("RPC failed");
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

    // Start the RPC.
    let (mut tx, mut rx) = client.route_chat().await;

    // Spawn a task to send the request messages asynchronously.
    let handle = task::spawn(async move {
        // Send the request messages.
        for note in notes {
            // Send errors (with a void error) if the stream encounters any
            // problem, or if the server terminates the stream before the client
            // is done sending.  We don't expect that in our RouteGuide
            // protocol, so we can safely expect() no error.
            tx.send(note).await.expect("RPC terminated early");
        }
        // Send a "half close" signal to the server to indicate the client is
        // done sending.  This triggers naturally if `tx` is dropped, which will
        // happen automatically at the end of this task, but we call close()
        // here just to be explicit.
        tx.close();
    });

    while let Some(response) = rx.recv().await {
        println!(
            "Got message {} at Point: {{{}, {}}}",
            response.message(),
            response.location().latitude(),
            response.location().longitude()
        );
    }

    // Assert that spawned task did not encounter an error.
    handle.await.expect("Sending notes failed");

    // Confirm the status.
    let status = rx.status().await;
    assert_eq!(status.code(), grpc::StatusCode::Ok, "{:?}", status);
}

fn random_point() -> Point {
    let latitude = (rand::random_range(0..180) - 90) * 10_000_000;
    let longitude = (rand::random_range(0..360) - 180) * 10_000_000;
    return proto!(Point {
        latitude,
        longitude
    });
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
