# gRPC Basics: Tonic

This tutorial, adapted from [grpc-go], provides a basic introduction to working with gRPC
and Tonic. By walking through this example you'll learn how to:

- Define a service in a `.proto` file.
- Generate server and client code.
- Write a simple client and server for your service.

It assumes you are familiar with [protocol buffers] and basic Rust. Note that the example in
this tutorial uses the proto3 version of the protocol buffers language, you can find out more in the
[proto3 language guide][proto3].

[grpc-go]: https://github.com/grpc/grpc-go/blob/master/examples/gotutorial.md
[protocol buffers]: https://developers.google.com/protocol-buffers/docs/overview
[proto3]: https://developers.google.com/protocol-buffers/docs/proto3

## Why use gRPC?

Our example is a simple route mapping application that lets clients get information about features
on their route, create a summary of their route, and exchange route information such as traffic
updates with the server and other clients.

With gRPC we can define our service once in a `.proto` file and implement clients and servers in
any of gRPC's supported languages, which in turn can be run in environments ranging from servers
inside Google to your own tablet - all the complexity of communication between different languages
and environments is handled for you by gRPC. We also get all the advantages of working with
protocol buffers, including efficient serialization, a simple IDL, and easy interface updating.

## Prerequisites

To run the sample code and walk through the tutorial, the only prerequisite is Rust itself.
[rustup] is a convenient tool to install it, if you haven't already.

[rustup]: https://rustup.rs

## Running the example

Clone or download Tonic's repository:

```shell
$ git clone https://github.com/hyperium/tonic.git
```

Change your current directory to Tonic's repository root:
```shell
$ cd tonic
```

Tonic uses `rustfmt` to tidy up the code it generates, so we'll make sure it's installed.

```shell
$ rustup component add rustfmt
```

Run the server
```shell
$ cargo run --bin routeguide-server
```

In a separate shell, run the client
```shell
$ cargo run --bin routeguide-client
```

You should see some logging output flying past really quickly on both terminal windows. On the
shell where you ran the client binary, you should see the output of the bidirectional streaming rpc,
printing 1 line per second:

```
NOTE = RouteNote { location: Some(Point { latitude: 409146139, longitude: -746188906 }), message: "at 1.000319208s" }
```

If you scroll up you should see the output of the other 3 request types: simple rpc, server-side
streaming and client-side streaming.


## Project setup

We will develop our example from scratch in a new crate:

```shell
$ cargo new routeguide
$ cd routeguide
```


## Defining the service

Our first step is to define the gRPC *service* and the method *request* and *response* types using
[protocol buffers]. We will keep our `.proto` files in a directory in our crate's root.
Note that Tonic does not really care where our `.proto` definitions live. We will see how to use
different [code generation configuration](#tonic-build) later in the tutorial.

```shell
$ mkdir proto && touch proto/route_guide.proto
```

You can see the complete `.proto` file in
[examples/proto/routeguide/route_guide.proto][routeguide-proto].

To define a service, you specify a named `service` in your `.proto` file:

```proto
service RouteGuide {
   ...
}
```

Then you define `rpc` methods inside your service definition, specifying their request and response
types. gRPC lets you define four kinds of service method, all of which are used in the `RouteGuide`
service:

- A *simple RPC* where the client sends a request to the server and waits for a response to come
back, just like a normal function call.
```proto
   // Obtains the feature at a given position.
   rpc GetFeature(Point) returns (Feature) {}
```

- A *server-side streaming RPC* where the client sends a request to the server and gets a stream
to read a sequence of messages back. The client reads from the returned stream until there are
no more messages. As you can see in our example, you specify a server-side streaming method by
placing the `stream` keyword before the *response* type.
```proto
  // Obtains the Features available within the given Rectangle.  Results are
  // streamed rather than returned at once (e.g. in a response message with a
  // repeated field), as the rectangle may cover a large area and contain a
  // huge number of features.
  rpc ListFeatures(Rectangle) returns (stream Feature) {}
```

- A *client-side streaming RPC* where the client writes a sequence of messages and sends them to
the server. Once the client has finished writing the messages, it waits for the server to read them
all and return its response. You specify a client-side streaming method by placing the `stream`
keyword before the *request* type.
```proto
  // Accepts a stream of Points on a route being traversed, returning a
  // RouteSummary when traversal is completed.
  rpc RecordRoute(stream Point) returns (RouteSummary) {}
```

- A *bidirectional streaming RPC* where both sides send a sequence of messages. The two streams
operate independently, so clients and servers can read and write in whatever
order they like: for example, the server could wait to receive all the client messages before
writing its responses, or it could alternately read a message then write a message, or some other
combination of reads and writes. The order of messages in each stream is preserved. You specify
this type of method by placing the `stream` keyword before both the request and the response.
```proto
  // Accepts a stream of RouteNotes sent while a route is being traversed,
  // while receiving other RouteNotes (e.g. from other users).
  rpc RouteChat(stream RouteNote) returns (stream RouteNote) {}
```

Our `.proto` file also contains protocol buffer message type definitions for all the request and
response types used in our service methods - for example, here's the `Point` message type:
```proto
// Points are represented as latitude-longitude pairs in the E7 representation
// (degrees multiplied by 10**7 and rounded to the nearest integer).
// Latitudes should be in the range +/- 90 degrees and longitude should be in
// the range +/- 180 degrees (inclusive).
message Point {
  int32 latitude = 1;
  int32 longitude = 2;
}
```

[routeguide-proto]: https://github.com/hyperium/tonic/blob/master/examples/proto/routeguide/route_guide.proto

## Generating client and server code

Tonic can be configured to generate code as part cargo's normal build process. This is very
convenient because once we've set everything up, there is no extra step to keep the generated code
and our `.proto` definitions in sync.

Behind the scenes, Tonic uses [PROST!] to handle protocol buffer serialization and code
generation.

Edit `Cargo.toml` and add all the dependencies we'll need for this example:

```toml
[dependencies]
tonic = "0.4"
prost = "0.7"
futures-core = "0.3"
futures-util = "0.3"
tokio = { version = "1.0", features = ["rt-multi-thread", "macros", "sync", "time"] }

async-stream = "0.2"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
rand = "0.7"

[build-dependencies]
tonic-build = "0.4"
```

Create a `build.rs` file at the root of your crate:

```rust
fn main() {
    tonic_build::compile_protos("proto/route_guide.proto")
        .unwrap_or_else(|e| panic!("Failed to compile protos {:?}", e));
}
```

```shell
$ cargo build
```

That's it. The generated code contains:

- Struct definitions for message types `Point`, `Rectangle`, `Feature`, `RouteNote`, `RouteSummary`.
- A service trait we'll need to implement: `route_guide_server::RouteGuide`.
- A client type we'll use to call the server: `route_guide_client::RouteGuideClient<T>`.

If your are curious as to where the generated files are, keep reading. The mystery will be revealed
soon! We can now move on to the fun part.

[PROST!]: https://github.com/danburkert/prost

## Creating the server

First let's look at how we create a `RouteGuide` server. If you're only interested in creating gRPC
clients, you can skip this section and go straight to [Creating the client](#client)
(though you might find it interesting anyway!).

There are two parts to making our `RouteGuide` service do its job:

- Implementing the service trait generated from our service definition.
- Running a gRPC server to listen for requests from clients.

You can find our example `RouteGuide` server in
[examples/src/routeguide/server.rs][routeguide-server].

[routeguide-server]: https://github.com/hyperium/tonic/blob/master/examples/src/routeguide/server.rs

### Implementing the RouteGuide server trait

We can start by defining a struct to represent our service, we can do this on `main.rs` for now:

```rust
#[derive(Debug)]
struct RouteGuideService;
```

Next, we need to implement the `route_guide_server::RouteGuide` trait that is generated in our build step.
The generated code is placed inside our target directory, in a location defined by the `OUT_DIR`
environment variable that is set by cargo. For our example, this means you can find the generated
code in a path similar to `target/debug/build/routeguide/out/routeguide.rs`.

You can learn more about `build.rs` and the `OUT_DIR` environment variable in the [cargo book].

We can use Tonic's `include_proto` macro to bring the generated code into scope:

```rust
pub mod routeguide {
    tonic::include_proto!("routeguide");
}

use routeguide::route_guide_server::{RouteGuide, RouteGuideServer};
use routeguide::{Feature, Point, Rectangle, RouteNote, RouteSummary};
```

**Note**: The token passed to the `include_proto` macro (in our case "routeguide") is the name of
the package declared in in our `.proto` file, not a filename, e.g "routeguide.rs".

With this in place, we can stub out our service implementation:

```rust
use futures_core::Stream;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;
use tonic::{Request, Response, Status};
```

```rust
#[tonic::async_trait]
impl RouteGuide for RouteGuideService {
    async fn get_feature(&self, _request: Request<Point>) -> Result<Response<Feature>, Status> {
        unimplemented!()
    }

    type ListFeaturesStream = mpsc::Receiver<Result<Feature, Status>>;

    async fn list_features(
        &self,
        _request: Request<Rectangle>,
    ) -> Result<Response<Self::ListFeaturesStream>, Status> {
        unimplemented!()
    }

    async fn record_route(
        &self,
        _request: Request<tonic::Streaming<Point>>,
    ) -> Result<Response<RouteSummary>, Status> {
        unimplemented!()
    }

    type RouteChatStream = Pin<Box<dyn Stream<Item = Result<RouteNote, Status>> + Send + Sync + 'static>>;

    async fn route_chat(
        &self,
        _request: Request<tonic::Streaming<RouteNote>>,
    ) -> Result<Response<Self::RouteChatStream>, Status> {
        unimplemented!()
    }
}
```

**Note**: The `tonic::async_trait` attribute macro adds support for async functions in traits. It
uses [async-trait] internally. You can learn more about `async fn` in traits in the [async book].


[cargo book]: https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts
[async-trait]: https://github.com/dtolnay/async-trait
[async book]: https://rust-lang.github.io/async-book/07_workarounds/05_async_in_traits.html

### Server state
Our service needs access to an immutable list of features. When the server starts, we are going to
deserialize them from a json file and keep them around as our only piece of shared state:

```rust
#[derive(Debug)]
pub struct RouteGuideService {
    features: Arc<Vec<Feature>>,
}
```

Create the json data file and a helper module to read and deserialize our features.

```shell
$ mkdir data && touch data/route_guide_db.json
$ touch src/data.rs
```

You can find our example json data in [examples/data/route_guide_db.json][route-guide-db] and
the corresponding `data` module to load and deserialize it in
[examples/routeguide/data.rs][data-module].

**Note:** If you are following along, you'll need to change the data file's path  from
`examples/data/route_guide_db.json` to `data/route_guide_db.json`.

Next, we need to implement `Hash` and `Eq` for `Point`, so we can use point values as map keys:

```rust
use std::hash::{Hasher, Hash};
```

```rust
impl Hash for Point {
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.latitude.hash(state);
        self.longitude.hash(state);
    }
}

impl Eq for Point {}

```

Lastly, we need implement two helper functions: `in_range` and `calc_distance`. We'll use them
when performing feature lookups. You can find them in
[examples/src/routeguide/server.rs][in-range-fn].

[route-guide-db]: https://github.com/hyperium/tonic/blob/master/examples/data/route_guide_db.json
[data-module]: https://github.com/hyperium/tonic/blob/master/examples/src/routeguide/data.rs
[in-range-fn]: https://github.com/hyperium/tonic/blob/master/examples/src/routeguide/server.rs#L174

#### Request and Response types
All our service methods receive a `tonic::Request<T>` and return a
`Result<tonic::Response<T>, tonic::Status>`. The concrete type of `T` depends on how our methods
are declared in our *service* `.proto` definition. It can be either:

- A single value, e.g `Point`, `Rectangle`, or even a message type that includes a repeated field.
- A stream of values, e.g. `impl Stream<Item = Result<Feature, tonic::Status>>`.

#### Simple RPC
Let's look at the simplest method first, `get_feature`, which just gets a `tonic::Request<Point>`
from the client and tries to find a feature at the given `Point`. If no feature is found, it returns
an empty one.

```rust
async fn get_feature(&self, request: Request<Point>) -> Result<Response<Feature>, Status> {
    for feature in &self.features[..] {
        if feature.location.as_ref() == Some(request.get_ref()) {
            return Ok(Response::new(feature.clone()));
        }
    }

    Ok(Response::new(Feature::default()))
}
```


#### Server-side streaming RPC
Now let's look at one of our streaming RPCs. `list_features` is a server-side streaming RPC, so we
need to send back multiple `Feature`s to our client.

```rust
type ListFeaturesStream = mpsc::Receiver<Result<Feature, Status>>;

async fn list_features(
    &self,
    request: Request<Rectangle>,
) -> Result<Response<Self::ListFeaturesStream>, Status> {
    let (mut tx, rx) = mpsc::channel(4);
    let features = self.features.clone();

    tokio::spawn(async move {
        for feature in &features[..] {
            if in_range(feature.location.as_ref().unwrap(), request.get_ref()) {
                tx.send(Ok(feature.clone())).await.unwrap();
            }
        }
    });

    Ok(Response::new(rx))
}
```

Like `get_feature`, `list_features`'s input is a single message, a `Rectangle` in this
case. This time, however, we need to return a stream of values, rather than a single one.
We create a channel and spawn a new asynchronous task where we perform a lookup, sending
the features that satisfy our constraints into the channel.

The `Stream` half of the channel is returned to the caller, wrapped in a `tonic::Response`.


#### Client-side streaming RPC
Now let's look at something a little more complicated: the client-side streaming method
`record_route`, where we get a stream of `Point`s from the client and return a single `RouteSummary`
with information about their trip. As you can see, this time the method receives a
`tonic::Request<tonic::Streaming<Point>>`.

```rust
use std::time::Instant;
use futures_util::StreamExt;
```

```rust
async fn record_route(
    &self,
    request: Request<tonic::Streaming<Point>>,
) -> Result<Response<RouteSummary>, Status> {
    let mut stream = request.into_inner();

    let mut summary = RouteSummary::default();
    let mut last_point = None;
    let now = Instant::now();

    while let Some(point) = stream.next().await {
        let point = point?;
        summary.point_count += 1;

        for feature in &self.features[..] {
            if feature.location.as_ref() == Some(&point) {
                summary.feature_count += 1;
            }
        }

        if let Some(ref last_point) = last_point {
            summary.distance += calc_distance(last_point, &point);
        }

        last_point = Some(point);
    }

    summary.elapsed_time = now.elapsed().as_secs() as i32;

    Ok(Response::new(summary))
}
```

`record_route` is conceptually simple: we get a stream of `Points` and fold it into a `RouteSummary`.
In other words, we build a summary value as we process each `Point` in our stream, one by one.
When there are no more `Points` in our stream, we return the `RouteSummary` wrapped in a
`tonic::Response`.

#### Bidirectional streaming RPC
Finally, let's look at our bidirectional streaming RPC `route_chat`, which receives a stream
of `RouteNote`s and returns  a stream of `RouteNote`s.

```rust
use std::collections::HashMap;
```

```rust
type RouteChatStream =
    Pin<Box<dyn Stream<Item = Result<RouteNote, Status>> + Send + Sync + 'static>>;


async fn route_chat(
    &self,
    request: Request<tonic::Streaming<RouteNote>>,
) -> Result<Response<Self::RouteChatStream>, Status> {
    let mut notes = HashMap::new();
    let mut stream = request.into_inner();

    let output = async_stream::try_stream! {
        while let Some(note) = stream.next().await {
            let note = note?;

            let location = note.location.clone().unwrap();

            let location_notes = notes.entry(location).or_insert(vec![]);
            location_notes.push(note);

            for note in location_notes {
                yield note.clone();
            }
        }
    };

    Ok(Response::new(Box::pin(output)
        as Self::RouteChatStream))

}
```

`route_chat` uses the [async-stream] crate to perform an asynchronous transformation
from one (input) stream to another (output) stream. As the input is processed, each value is
inserted into the notes map, yielding a clone of the original `RouteNote`. The resulting stream
is then returned to the caller. Neat.

**Note**: The funky `as` cast is needed due to a limitation in the rust compiler. This is expected
to be fixed soon.

[async-stream]: https://github.com/tokio-rs/async-stream

### Starting the server

Once we've implemented all our methods, we also need to start up a gRPC server so that clients can
actually use our service. This is how our `main` function looks like:

```rust
mod data;
use tonic::transport::Server;
```

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:10000".parse().unwrap();

    let route_guide = RouteGuideService {
        features: Arc::new(data::load()),
    };

    let svc = RouteGuideServer::new(route_guide);

    Server::builder().add_service(svc).serve(addr).await?;

    Ok(())
}
```

To handle requests, `Tonic` uses [Tower] and [hyper] internally. What this means,
among other things, is that we have a flexible and composable stack we can build on top of. We can,
for example, add an [interceptor][authentication-example] to process requests before they reach our service
methods.


[Tower]: https://github.com/tower-rs
[hyper]: https://github.com/hyperium/hyper
[authentication-example]: https://github.com/hyperium/tonic/blob/master/examples/src/authentication/server.rs#L56

<a name="client"></a>
## Creating the client

In this section, we'll look at creating a Tonic client for our `RouteGuide` service. You can see our
complete example client code in [examples/src/routeguide/client.rs][routeguide-client].

Our crate will have two binary targets: `routeguide-client` and `routeguide-server`. We need to
edit our `Cargo.toml` accordingly:

```toml
[[bin]]
name = "routeguide-server"
path = "src/server.rs"

[[bin]]
name = "routeguide-client"
path = "src/client.rs"
```

Rename `main.rs` to `server.rs` and create a new file `client.rs`.

```shell
$ mv src/main.rs src/server.rs
$ touch src/client.rs
```

To call service methods, we first need to create a gRPC *client* to communicate with the server. Like in the server
case, we'll start by bringing the generated code into scope:

```rust
pub mod routeguide {
    tonic::include_proto!("routeguide");
}

use routeguide::route_guide_client::RouteGuideClient;
use routeguide::{Point, Rectangle, RouteNote};


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = RouteGuideClient::connect("http://[::1]:10000").await?;

     Ok(())
}
```

Same as in the server implementation, we start by bringing our generated code into scope. We then
create a client in our main function, passing the server's full URL to `RouteGuideClient::connect`.
Our client is now ready to make service calls. Note that `client` is mutable, this is because it
needs to manage internal state.

[routeguide-client]: https://github.com/hyperium/tonic/blob/master/examples/src/routeguide/client.rs


### Calling service methods
Now let's look at how we call our service methods. Note that in Tonic, RPCs are asynchronous,
which means that RPC calls need to be `.await`ed.

#### Simple RPC
Calling the simple RPC `get_feature` is as straightforward as calling a local method:

```rust
use tonic::Request;
```

```rust
let response = client
    .get_feature(Request::new(Point {
        latitude: 409146138,
        longitude: -746188906,
    }))
    .await?;

println!("RESPONSE = {:?}", response);
```
We call the `get_feature` client method, passing a single `Point` value wrapped in a
`tonic::Request`. We get a `Result<tonic::Response<Feature>, tonic::Status>` back.

#### Server-side streaming RPC
Here's where we call the server-side streaming method `list_features`, which returns a stream of
geographical `Feature`s.

```rust
use tonic::transport::Channel;
use std::error::Error;
```

```rust
async fn print_features(client: &mut RouteGuideClient<Channel>) -> Result<(), Box<dyn Error>> {
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

    let mut stream = client
        .list_features(Request::new(rectangle))
        .await?
        .into_inner();

    while let Some(feature) = stream.message().await? {
        println!("NOTE = {:?}", feature);
    }

    Ok(())
}
```

As in the simple RPC, we pass a single value request. However, instead of getting a
single value back, we get a stream of `Features`.

We use the the `message()` method from the `tonic::Streaming` struct to repeatedly read in the
server's responses to a response protocol buffer object (in this case a `Feature`) until there are
no more messages left in the stream.

#### Client-side streaming RPC
The client-side streaming method `record_route` takes a stream of `Point`s and returns a single
`RouteSummary` value.

```rust
use rand::rngs::ThreadRng;
use rand::Rng;
use futures_util::stream;
```

```rust
async fn run_record_route(client: &mut RouteGuideClient<Channel>) -> Result<(), Box<dyn Error>> {
    let mut rng = rand::thread_rng();
    let point_count: i32 = rng.gen_range(2..100);

    let mut points = vec![];
    for _ in 0..=point_count {
        points.push(random_point(&mut rng))
    }

    println!("Traversing {} points", points.len());
    let request = Request::new(stream::iter(points));

    match client.record_route(request).await {
        Ok(response) => println!("SUMMARY: {:?}", response.into_inner()),
        Err(e) => println!("something went wrong: {:?}", e),
    }

    Ok(())
}
```

```rust
fn random_point(rng: &mut ThreadRng) -> Point {
    let latitude = (rng.gen_range(0..180) - 90) * 10_000_000;
    let longitude = (rng.gen_range(0..360) - 180) * 10_000_000;
    Point {
        latitude,
        longitude,
    }
}
```

We build a vector of a random number of `Point` values (between 2 and 100) and then convert
it into a `Stream` using the `futures::stream::iter` function. This is a cheap an easy way to get
a stream suitable for passing into our service method. The resulting stream is then wrapped in a
`tonic::Request`.


#### Bidirectional streaming RPC

Finally, let's look at our bidirectional streaming RPC. The `route_chat` method takes a stream
of `RouteNotes` and returns either another stream of `RouteNotes` or an error.

```rust
use std::time::Duration;
use tokio::time;
```

```rust
async fn run_route_chat(client: &mut RouteGuideClient<Channel>) -> Result<(), Box<dyn Error>> {
    let start = time::Instant::now();

    let outbound = async_stream::stream! {
        let mut interval = time::interval(Duration::from_secs(1));

        while let time = interval.tick().await {
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
```
In this case, we use the [async-stream] crate to generate our outbound stream, yielding
`RouteNote` values in one second intervals. We then iterate over the stream returned by
the server, printing each value in the stream.

## Try it out!

### Run the server
```shell
$ cargo run --bin routeguide-server
```

### Run the client
```shell
$ cargo run --bin routeguide-client
```

## Appendix

<a name="tonic-build"></a>
### tonic_build configuration

Tonic's default code generation configuration is convenient for self contained examples and small
projects. However, there are some cases when we need a slightly different workflow. For example:

- When building rust clients and servers in different crates.
- When building a rust client or server (or both) as part of a larger, multi-language project.
- When we want editor support for the generate code and our editor does not index the generated
files in the default location.

More generally, whenever we want to keep our `.proto` definitions in a central place and generate
code for different crates or different languages, the default configuration is not enough.

Luckily, `tonic_build` can be configured to fit whatever workflow we need. Here are just two
possibilities:

1)  We can keep our `.proto` definitions in a separate crate and generate our code on demand, as
opposed to at build time, placing the resulting modules wherever we need them.

`main.rs`

```rust
fn main() {
    tonic_build::configure()
        .build_client(false)
        .out_dir("another_crate/src/pb")
        .compile(&["path/my_proto.proto"], &["path"])
        .expect("failed to compile protos");
}
```

On `cargo run`, this will generate code for the client only, and place the resulting file in
`another_crate/src/pb`.

2) Similarly, we could also keep the `.proto` definitions in a separate crate and then use that
crate as a direct dependency wherever we need it.

