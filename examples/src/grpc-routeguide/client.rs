/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

use protobuf::proto;

#[allow(unused, clippy::all)]
mod generated {
    pub mod routeguide {
        grpc::include_generated_proto!("generated/routeguide", "route_guide");
    }
}

use std::env;
use std::sync::Arc;

use grpc::client::Channel;
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
    assert!(status.is_ok(), "{:?}", status);
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
        if stream.send(point).await.is_err() {
            // RPC error; break to read the status.
            break;
        }
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
            // Send errors (with a unit error) if the stream encounters any
            // problem, or if the server terminates the stream before the client
            // is done sending.  The response stream will provide the RPC status
            // in this case.
            if tx.send(note).await.is_err() {
                return;
            }
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

    // Confirm the status.
    let status = rx.status().await;
    assert!(status.is_ok(), "{:?}", status);

    // Wait for the spawned task to complete as part of proper resource cleanup.
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
    let args: Vec<String> = env::args().collect();
    let address = if args.len() > 1 {
        args[1].clone()
    } else {
        "[::1]:10000".to_owned()
    };
    println!("Connecting to {address}...");

    // Create a new gRPC channel:
    let channel = Channel::builder(
        format!("dns:///{address}"),
        Arc::new(LocalChannelCredentials::new()),
    )
    .build();

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
