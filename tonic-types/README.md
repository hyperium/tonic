# tonic-types

A collection of useful protobuf types that can be used with `tonic`.

This crate also introduces the [`StatusExt`] trait and implements it in
[`tonic::Status`], allowing the implementation of the [gRPC Richer Error Model] 
with [`tonic`] in a convenient way.

## Usage

Useful protobuf types are available through the [`pb`] module. They can be
imported and worked with directly.  

The [`StatusExt`] trait adds associated functions to [`tonic::Status`] that can
be used on the server side to create a status with error details, which can then
be returned to gRPC clients. Moreover, the trait also adds methods to
[`tonic::Status`] that can be used by a tonic client to extract error details,
and handle them with ease.

## Getting Started

```toml
[dependencies]
tonic = <tonic-version>
tonic-types = <tonic-types-version>
```

## Examples

The examples bellow cover a basic use case of the [gRPC Richer Error Model].
More complete server and client implementations are provided in the
**Richer Error example**, located in the main repo [examples] directory.

### Server Side: Generating [`tonic::Status`] with an [`ErrorDetails`] struct

```rust
use tonic::{Code, Status};
use tonic_types::{ErrorDetails, StatusExt};

// ...
// Inside a gRPC server endpoint that returns `Result<Response<T>, Status>`

// Create empty `ErrorDetails` struct
let mut err_details = ErrorDetails::new();

// Add error details conditionally
if some_condition {
    err_details.add_bad_request_violation(
        "field_a",
        "description of why the field_a is invalid"
    );
}

if other_condition {
    err_details.add_bad_request_violation(
        "field_b",
        "description of why the field_b is invalid",
    );
}

// Check if any error details were set and return error status if so
if err_details.has_bad_request_violations() {
    // Add additional error details if necessary
    err_details
        .add_help_link("description of link", "https://resource.example.local")
        .set_localized_message("en-US", "message for the user");

    let status = Status::with_error_details(
        Code::InvalidArgument,
        "bad request",
        err_details,
    );
    return Err(status);
}

// Handle valid request
// ...
```

### Client Side: Extracting an [`ErrorDetails`] struct from [`tonic::Status`]

```rust
use tonic::{Response, Status};
use tonic_types::StatusExt;

// ...
// Where `req_result` was returned by a gRPC client endpoint method
fn handle_request_result<T>(req_result: Result<Response<T>, Status>) {
    match req_result {
        Ok(response) => {
            // Handle successful response
        },
        Err(status) => {
            let err_details = status.get_error_details();
            if let Some(bad_request) = err_details.bad_request() {
                // Handle bad_request details
            }
            if let Some(help) = err_details.help() {
                // Handle help details
            }
            if let Some(localized_message) = err_details.localized_message() {
                // Handle localized_message details
            }
        }
    };
}
```

## Working with different error message types

Multiple examples are provided at the [`ErrorDetails`] doc. Instructions about 
how to use the fields of the standard error message types correctly are provided
at [error_details.proto].

## Alternative `tonic::Status` associated functions and methods

In the [`StatusExt`] doc, an alternative way of interacting with
[`tonic::Status`] is presented, using vectors of error details structs wrapped
with the [`ErrorDetail`] enum. This approach can provide more control over the
vector of standard error messages that will be generated or that was received,
if necessary. To see how to adopt this approach, please check the
[`StatusExt::with_error_details_vec`] and [`StatusExt::get_error_details_vec`]
docs, and also the main repo's [Richer Error example] directory.  

Besides that, multiple examples with alternative error details extraction
methods are provided in the [`StatusExt`] doc, which can be specially
useful if only one type of standard error message is being handled by the
client. For example, using [`StatusExt::get_details_bad_request`] is a
more direct way of extracting a [`BadRequest`] error message from
[`tonic::Status`].

[`tonic::Status`]: https://docs.rs/tonic/latest/tonic/struct.Status.html
[`tonic`]: https://docs.rs/tonic/latest/tonic/
[gRPC Richer Error Model]: https://www.grpc.io/docs/guides/error/
[`pb`]: https://docs.rs/tonic-types/latest/tonic_types/pb/index.html
[`StatusExt`]: https://docs.rs/tonic-types/latest/tonic_types/trait.StatusExt.html
[examples]: https://github.com/hyperium/tonic/tree/master/examples
[`ErrorDetails`]: https://docs.rs/tonic-types/latest/tonic_types/struct.ErrorDetails.html
[error_details.proto]: https://github.com/googleapis/googleapis/blob/master/google/rpc/error_details.proto
[`ErrorDetail`]: https://docs.rs/tonic-types/latest/tonic_types/enum.ErrorDetail.html
[`StatusExt::with_error_details_vec`]: https://docs.rs/tonic-types/latest/tonic_types/trait.StatusExt.html#tymethod.with_error_details_vec
[`StatusExt::get_error_details_vec`]: https://docs.rs/tonic-types/latest/tonic_types/trait.StatusExt.html#tymethod.get_error_details_vec
[Richer Error example]: https://github.com/hyperium/tonic/tree/master/examples/src/richer-error
[`StatusExt::get_details_bad_request`]: https://docs.rs/tonic-types/latest/tonic_types/trait.StatusExt.html#tymethod.get_details_bad_request
[`BadRequest`]: https://docs.rs/tonic-types/latest/tonic_types/struct.BadRequest.html