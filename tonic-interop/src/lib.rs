#![recursion_limit = "256"]

pub mod client;
pub mod server;

pub mod pb {
    #![allow(dead_code)]
    #![allow(unused_imports)]
    include!(concat!(env!("OUT_DIR"), "/grpc.testing.rs"));
}

use http::header::{HeaderMap, HeaderName, HeaderValue};
use http_body::Body;
use std::{
    default, fmt, iter,
    pin::Pin,
    task::{Context, Poll},
};

pub fn trace_init() {
    let sub = tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(tracing_subscriber::filter::EnvFilter::from_default_env())
        .finish();

    let _ = tracing::subscriber::set_global_default(sub);
    let _ = tracing_log::LogTracer::init();
}

pub fn client_payload(size: usize) -> pb::Payload {
    pb::Payload {
        r#type: default::Default::default(),
        body: iter::repeat(0u8).take(size).collect(),
    }
}

pub fn server_payload(size: usize) -> pb::Payload {
    pb::Payload {
        r#type: default::Default::default(),
        body: iter::repeat(0u8).take(size).collect(),
    }
}

impl pb::ResponseParameters {
    fn with_size(size: i32) -> Self {
        pb::ResponseParameters {
            size,
            ..Default::default()
        }
    }
}

fn response_length(response: &pb::StreamingOutputCallResponse) -> i32 {
    match &response.payload {
        Some(ref payload) => payload.body.len() as i32,
        None => 0,
    }
}

fn response_lengths(responses: &Vec<pb::StreamingOutputCallResponse>) -> Vec<i32> {
    responses.iter().map(&response_length).collect()
}

#[derive(Debug)]
pub enum TestAssertion {
    Passed {
        description: &'static str,
    },
    Failed {
        description: &'static str,
        expression: &'static str,
        why: Option<String>,
    },
}

impl TestAssertion {
    pub fn is_failed(&self) -> bool {
        match self {
            TestAssertion::Failed { .. } => true,
            _ => false,
        }
    }
}

impl fmt::Display for TestAssertion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use console::{style, Emoji};
        match *self {
            TestAssertion::Passed { ref description } => write!(
                f,
                "{check} {desc}",
                check = style(Emoji("✔", "+")).green(),
                desc = style(description).green(),
            ),
            TestAssertion::Failed {
                ref description,
                ref expression,
                why: Some(ref why),
            } => write!(
                f,
                "{check} {desc}\n  in `{exp}`: {why}",
                check = style(Emoji("✖", "x")).red(),
                desc = style(description).red(),
                exp = style(expression).red(),
                why = style(why).red(),
            ),
            TestAssertion::Failed {
                ref description,
                ref expression,
                why: None,
            } => write!(
                f,
                "{check} {desc}\n  in `{exp}`",
                check = style(Emoji("✖", "x")).red(),
                desc = style(description).red(),
                exp = style(expression).red(),
            ),
        }
    }
}

#[macro_export]
macro_rules! test_assert {
    ($description:expr, $assertion:expr) => {
        if $assertion {
            crate::TestAssertion::Passed {
                description: $description,
            }
        } else {
            TestAssertion::Failed {
                description: $description,
                expression: stringify!($assertion),
                why: None,
            }
        }
    };
    ($description:expr, $assertion:expr, $why:expr) => {
        if $assertion {
            crate::TestAssertion::Passed {
                description: $description,
            }
        } else {
            crate::TestAssertion::Failed {
                description: $description,
                expression: stringify!($assertion),
                why: Some($why),
            }
        }
    };
}

pub struct MergeTrailers<B> {
    inner: B,
    trailer: Option<(HeaderName, HeaderValue)>,
}

impl<B> MergeTrailers<B> {
    pub fn new(inner: B, trailer: Option<(HeaderName, HeaderValue)>) -> Self {
        Self { inner, trailer }
    }
}

impl<B: Body + Unpin> Body for MergeTrailers<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_data(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        Pin::new(&mut self.inner).poll_data(cx)
    }

    fn poll_trailers(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<HeaderMap>, Self::Error>> {
        Pin::new(&mut self.inner).poll_trailers(cx).map_ok(|h| {
            h.map(|mut headers| {
                if let Some((key, value)) = &self.trailer {
                    headers.insert(key.clone(), value.clone());
                }

                headers
            })
        })
    }
}
