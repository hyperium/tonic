# Google Cloud Pub/Sub Example

This example demonstrates how to fetch a list of topics using Google's gRPC
Protocol Buffers specification. The request is secured using an OAuth token and
TLS.

You must have the following binaries in your `PATH`:

1.  `protoc`: The [Protocol Buffers compiler] and the well-known `.proto` files
    bundled with it.
1.  `protoc-gen-rust-grpc`: The `protoc` plugin to generate service code.
    ([View instruction](https://github.com/hyperium/tonic/tree/master/protoc-gen-rust-grpc))

Next, ensure your environment has [Application Default Credentials] configured.
You can do this by setting the `GOOGLE_APPLICATION_CREDENTIALS` environment
variable, or by running the `gcloud auth application-default login` command.

Once your credentials are set up, you will need your GCP Project ID, which can
be found on the main dashboard of the Google Cloud Console. With both of these
ready, you can run the example like so:

```shell
cargo run --bin grpc-gcp-client -- <project-id>
```

[Application Default Credentials]: https://docs.cloud.google.com/docs/authentication/application-default-credentials
[protocol buffers compiler]: https://protobuf.dev/installation/
