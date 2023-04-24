#![recursion_limit = "256"]

pub mod client;
pub mod server;

pub mod pb {
    #![allow(dead_code)]
    #![allow(unused_imports)]
    include!(concat!(env!("OUT_DIR"), "/grpc.testing.rs"));
}

use std::{default, fmt, iter};

pub fn trace_init() {
    tracing_subscriber::fmt::init();
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

fn response_lengths(responses: &[pb::StreamingOutputCallResponse]) -> Vec<i32> {
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
        matches!(self, TestAssertion::Failed { .. })
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
            $crate::TestAssertion::Passed {
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
            $crate::TestAssertion::Passed {
                description: $description,
            }
        } else {
            $crate::TestAssertion::Failed {
                description: $description,
                expression: stringify!($assertion),
                why: Some($why),
            }
        }
    };
}
