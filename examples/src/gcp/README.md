# Google Cloud Pubsub example

This example will attempt to fetch a list of topics using the google
gRPC protobuf specification. This will use an OAuth token and TLS to
fetch the list of topics.

First, you must generate a access token via the [OAuth playground]. From here
select the `Cloud Pub/Sub API v1` and its urls as the scope. This will start
the OAuth flow. Then you must hit the `Exchange authorization code for tokens`
button to generate an `access_token` which will show up in the HTTP response
to the right under the `access_token` field in the response json.

Once, you have this token you must fetch your GCP project id which can be found
from the main page on the dashboard. When you have both of these items you can
run the example like so:

```shell
GCP_AUTH_TOKEN="<access-token>" cargo run --bin gcp-client -- <project-id>
```

[OAuth playground]: https://developers.google.com/oauthplayground/
