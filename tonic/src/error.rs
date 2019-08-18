use std::fmt;

#[allow(dead_code)]
pub type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug)]
#[allow(dead_code)]
pub enum Never {}

impl fmt::Display for Never {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {}
    }
}

impl std::error::Error for Never {}
