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

use std::{any::Any, pin::Pin};

use futures_core::Stream;
use tonic::{async_trait, Request as TonicRequest, Response as TonicResponse, Status};

pub type Request = TonicRequest<Pin<Box<dyn Stream<Item = Box<dyn Message>> + Send + Sync>>>;
pub type Response =
    TonicResponse<Pin<Box<dyn Stream<Item = Result<Box<dyn Message>, Status>> + Send + Sync>>>;

#[async_trait]
pub trait Service: Send + Sync {
    async fn call(&self, method: String, request: Request) -> Response;
}

// TODO: define methods that will allow serialization/deserialization.
pub trait Message: Any + Send + Sync {}
