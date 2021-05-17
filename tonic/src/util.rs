//! Various utilities used throughout tonic.

// some combinations of features might cause things here not to be used
#![allow(dead_code)]

use pin_project::pin_project;

/// A pin-project compatible `Option`
#[pin_project(project = OptionPinProj)]
pub(crate) enum OptionPin<T> {
    Some(#[pin] T),
    None,
}
