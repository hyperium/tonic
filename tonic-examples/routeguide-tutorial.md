# gRPC Basics: Tonic

This tutorial, adapted from [grpc-go][grpc-go], provides a basic introduction to working with gRPC
and tonic. By walking through this example you'll learn how to:

- Define a service in a `.proto` file.
- Generate server and client code.
- Write a simple client and server for your service.

It assumes you are familiar with [protocol buffers][protobuf] and Rust. Note that the example in
this tutorial uses the proto3 version of the protocol buffers language, you can find out more in the 
[proto3 language guide][proto3]. 

[grpc-go]: https://github.com/grpc/grpc-go/blob/master/examples/gotutorial.md
[protobuf]: https://developers.google.com/protocol-buffers/docs/overview 
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
[rustup][rustup] is a convenient tool to install it, if you haven't already.

[rustup]: https://rustup.rs
 
## Running the example

Clone or download Tonic's repository:

```shell 
git clone https://github.com/hyperium/tonic.git
```

Change your current directory to Tonic's repository root:
```shell
$ cd tonic
```

Tonic uses `rustfmt` to tidy up the code it generates, make sure it's installed.

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

**Note:** Prior to rust's 1.39 release, Tonic may be pinned to a specific toolchain version.

## Project setup

We will develop our example from scratch in a new crate:
 
```shell
$ cargo new routeguide
$ cd routeguide
```


## Defining the service

Our first step is to define the gRPC *service* and the method *request* and *response* types using 
[protocol buffers][protobuf]. We will keep our `.proto` files in a directory in our crate's root.
Note that Tonic does not really care where our `.proto` definitions live. We will see how to use
different code generation configuration later in the tutorial.


```shell
$ mkdir proto && touch proto/route_guide.proto
```

You can see the complete `.proto` file in
[tonic-examples/proto/routeguide/route_guide.proto][routeguide-proto].

[routeguide-proto]: https://github.com/hyperium/tonic/blob/master/tonic-examples/proto/routeguide/route_guide.proto

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


## Generating client and server code

Tonic can be configured to generate code as part cargo's normal build process. This is very
convenient because once we've set everything up, there is no extra step to keep the generated code
and our `.proto` definitions in sync.

Behind the scenes, Tonic uses [PROST!][prost] to handle protocol buffer serialization and code
generation.

Edit `Cargo.toml` to add all the dependencies we'll need for this example:

```toml
[dependencies]
tonic = "0.1.0-alpha.1"
futures-preview = { version = "0.3.0-alpha.19", default-features = false, features = ["alloc"]}
tokio = "0.2.0-alpha.6"
prost = "0.5"
bytes = "0.4"
serde_json = "1.0"
serde = { version = "1.0", features = ["derive"] }

[build-dependencies]
tonic-build = "0.1.0-alpha.1"
```

Create a `build.rs` file at the root of your crate:

```rust
fn main() {
    tonic_build::compile_protos("proto/route_guide.proto").unwrap();
}
```

[prost]: https://github.com/danburkert/prost

```shell
$ cargo build
```

That's it. The generated code contains:

- Struct definitions for message types `Point`, `Rectangle`, `Feature`, `RouteNote`, `RouteSummary`.
- A service trait we'll need to implement: `server::RouteGuide`.
- A client type we'll use to call the server: `client::RouteGuideClient<T>`.

If your are curious as to where the generated files are, keep reading. The mystery will be revealed.
We can now move on to the fun part.

## Creating the server

First let's look at how we create a `RouteGuide` server. If you're only interested in creating gRPC
clients, you can skip this section and go straight to [Creating the client](#client) 
(though you might find it interesting anyway!).

There are two parts to making our `RouteGuide` service do its job:

- Implementing the service trait generated from our service definition.
- Running a gRPC server to listen for requests from clients.

You can find our example `RouteGuide` server in 
[tonic-examples/src/routeguide/server.rs][routeguide-server]

[routeguide-server]: https://github.com/hyperium/tonic/blob/master/tonic-examples/src/routeguide/server.rs

### Implementing the server::RouteGuide trait

We can start by defining a struct to represent our service, we can do this on `main.rs` for now:

```rust
#[derive(Debug)]
struct RouteGuide;
```

We now need to implement the `server::RouteGuide` trait that is generated in our build step.
The generated code is placed inside our target directory, in a location defined by the `OUT_DIR`
environment variable that is set by cargo. For our example, this means you can find the generated
code in a path similar to `target/debug/build/routeguide/out/routeguide.rs`.

You can learn more about `build.rs` the `OUT_DIR` environment variable in the [cargo book][cargo-book].

We can bring this code into scope like this:

```rust
pub mod routeguide {
    tonic::include_proto!("routeguide");
}

use routeguide::{server, Feature, Point, Rectangle, RouteNote, RouteSummary};
```

[cargo-book]: https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts

Now we are ready to stub out our service:

```rust
#[tonic::async_trait]
impl server::RouteGuide for RouteGuide {
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

    type RouteChatStream = Pin<Box<dyn Stream<Item = Result<RouteNote, Status>> + Send + 'static>>;

    async fn route_chat(
        &self,
        _request: Request<tonic::Streaming<RouteNote>>,
    ) -> Result<Response<Self::RouteChatStream>, Status> {
        unimplemented!()
    }
}
```

**Note**: The `tonic::async_trait` attribute macro adds support for async fn in traits. It uses
[async-trait][async-trait] internally.

[async-trait]: https://github.com/dtolnay/async-trait

### Server state
There are two pieces of state our service needs to access: an immutable list of features and a
mutable map from points to route notes.

When designing our state shape, we must consider that our server will run in a multi-threaded Tokio
executor and that the `server::RouteGuide` trait has `Send + Sync + 'static` bounds.

This in one way we can represent our state:

```rust
#[derive(Debug)]
pub struct RouteGuide {
    state: State,
}

#[derive(Debug, Clone)]
struct State {
    features: Arc<Vec<Feature>>,
    notes: Arc<Mutex<HashMap<Point, Vec<RouteNote>>>>,
}
```

When our server boots, we are going to deserialize our features vector from a json file.
Create the data file and a helper module to read and deserialize our features.

```shell
$ mkdir data && touch data/route_guide_db.json
$ touch src/data.rs
```

You can find our example json data in [tonic-examples/data/route_guide_db.json][route-guide-db] and
the corresponding `data` module to load and deserialize it in
[tonic-examples/routeguide/data.rs][data-module]

Lastly, we need to implement `Hash` and `Eq` for `Point` so we can use `point` values as map keys.

[route-guide-db]: https://github.com/hyperium/tonic/blob/master/tonic-examples/data/route_guide_db.json
[data-module]: https://github.com/hyperium/tonic/blob/master/tonic-examples/src/routeguide/data.rs


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


#### Simple RPC
Let's look at the simplest method first, `get_feature`, which just gets a `tonic::Request<Point>` 
from the client and tries to find a feature at the location represented by the given `Point`.
If no feature is found, it returns an empty one.

```rust
async fn get_feature(&self, request: Request<Point>) -> Result<Response<Feature>, Status> {
    for feature in &self.state.features[..] {
        if feature.location.as_ref() == Some(request.get_ref()) {
            return Ok(Response::new(feature.clone()));
        }
    }

    let response = Response::new(Feature {
        name: "".to_string(),
        location: None,
    });

    Ok(response)
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

    let state = self.state.clone();

    tokio::spawn(async move {
        for feature in &state.features[..] {
            if in_range(feature.location.as_ref().unwrap(), request.get_ref()) {
                tx.send(Ok(feature.clone())).await.unwrap();
            }
        }
    });

    Ok(Response::new(rx))
}
```

Similar to `get_feature`, `list_features`'s input is a simple message type. A `Rectangle` in this
case. This time, however, we need to return a stream of values, rather than a single one. 


#### Client-side streaming RPC
Now let's look at something a little more complicated: the client-side streaming method 
`record_route`, where we get a stream of `Point`s from the client and return a single `RouteSummary` 
with information about their trip. As you can see, this time the method receives a 
`tonic::Request<tonic::Streaming<Point>>` 

```rust
async fn record_route(
    &self,
    request: Request<tonic::Streaming<Point>>,
) -> Result<Response<RouteSummary>, Status> {
    let stream = request.into_inner();
    futures::pin_mut!(stream);

    let mut summary = RouteSummary::default();
    let mut last_point = None;
    let now = Instant::now();

    while let Some(point) = stream.next().await {
        let point = point?;
        summary.point_count += 1;

        for feature in &self.state.features[..] {
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

#### Bidirectional streaming RPC
Finally, let's look at our bidirectional streaming RPC `route_chat`.
```rust
async fn route_chat(
    &self,
    request: Request<tonic::Streaming<RouteNote>>,
) -> Result<Response<Self::RouteChatStream>, Status> {
    println!("RouteChat");

    let stream = request.into_inner();
    let mut state = self.state.clone();

    let output = async_stream::try_stream! {
        futures::pin_mut!(stream);

        while let Some(note) = stream.next().await {
            let note = note?;

            let location = note.location.clone().unwrap();

            let mut notes = state.notes.lock().await;
            let notes = notes.entry(location).or_insert(vec![]);
            notes.push(note);

            for note in notes {
                yield note.clone();
            }
        }
    };

    Ok(Response::new(Box::pin(output)
        as Pin<
            Box<dyn Stream<Item = Result<RouteNote, Status>> + Send + 'static>,
        >))
    }
}
```

### Starting the server

Once we've implemented all our methods, we also need to start up a gRPC server so that clients can
actually use our service. The following snippet shows how we do this for our `RouteGuide` service:

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:10000".parse().unwrap();

    let route_guide = RouteGuide {
        state: State {
            features: Arc::new(data::load()),
            notes: Arc::new(Mutex::new(HashMap::new())),
        },
    };

    let svc = server::RouteGuideServer::new(route_guide);

    Server::builder().serve(addr, svc).await?;

    Ok(())
}
```

To build and start a server, we:

1. Specify the socket address to use to listen for client requests 
2. Create an instance of the gRPC server `RouteGuide {...}`, populating our state
3. Register our service implementation with the gRPC server `RouteGuideServer::new(...)`.
4. Call `Server::builder().serve(...)`  to do a blocking wait until the process is killed.


<a name="client"></a>
## Creating the client

In this section, we'll look at creating a Rust client for our `RouteGuide` service. You can see our
complete example client code in [tonic-examples/src/routeguide/client.rs][routeguide-client]


[routeguide-client]: https://github.com/hyperium/tonic/blob/master/tonic-examples/src/routeguide/client.rs

### Creating a client

To call service methods, we first need to create a gRPC *client* to communicate with the server. 
Creating a client is as simple as:

```rust
let mut client = RouteGuideClient::connect("http://[::1]:10000")?;
```

### Calling service methods
Now let's look at how we call our service methods. Note that in Tonic, RPCs are asynchronous, 
which means that the RPC call needs to be awaited.

#### Simple RPC
Calling the simple RPC `GetFeature` is nearly as straightforward as calling a local method.

```rust
let response = client
    .get_feature(Request::new(Point {
        latitude: 409146138,
        longitude: -746188906,
    }))
    .await?;
```
As you can see, we call the method on the client we got earlier. In our method parameters we create 
and populate a request protocol buffer object (in our case `Point`), and wrap it in a
`tonic::Request`

#### Server-side streaming RPC

#### Bidirectional streaming RPC

## Try it out!

### Run the server

### Run the client

## Appendix
### tonic-build configuration
### Well Known Types
