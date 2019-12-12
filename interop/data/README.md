# Tonic Testing Certificates

This directory contains certificates used for testing interop between Tonic's
implementation of gRPC and the Go implementation. Certificates are generated
using [`terraform`][tf].

To regenerate certificates for some reason, do the following:

1. Install Terraform 0.12 (or higher)
1. From the `cert-generator` directory, run:
    1. `terraform init`
    1. `terraform apply`

This will generate certificates and write them to the filesystem. The effective
version should be committed to git.

[tf]: https://terraform.io
