/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 */

//! The official Rust implementation of [gRPC], a high performance, open source,
//! universal RPC framework
//!
//! This version is in progress and not recommended for any production use.  All
//! APIs are unstable.  Proceed at your own risk.
//!
//! [gRPC]: https://grpc.io

#![allow(dead_code)]

pub mod client;
mod rt;
pub mod service;

pub(crate) mod attributes;
pub(crate) mod byte_str;
