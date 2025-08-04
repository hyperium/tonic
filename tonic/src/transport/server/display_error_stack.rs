use std::error::Error;
use std::fmt;
use std::fmt::{Display, Formatter};

pub(crate) struct DisplayErrorStack<'a>(pub(crate) &'a (dyn Error + 'static));

impl<'a> Display for DisplayErrorStack<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;
        let mut next = self.0.source();
        while let Some(err) = next {
            write!(f, ": {err}")?;
            next = err.source();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::transport::server::display_error_stack::DisplayErrorStack;
    use std::error::Error;
    use std::fmt;
    use std::fmt::{Display, Formatter};
    use std::sync::Arc;

    #[test]
    fn test_display_error_stack() {
        #[derive(Debug)]
        struct TestError(&'static str, Option<Arc<TestError>>);

        impl Display for TestError {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl Error for TestError {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                self.1.as_ref().map(|e| e as &(dyn Error + 'static))
            }
        }

        let a = Arc::new(TestError("a", None));
        let b = Arc::new(TestError("b", Some(a.clone())));
        let c = Arc::new(TestError("c", Some(b.clone())));

        assert_eq!("a", DisplayErrorStack(&a).to_string());
        assert_eq!("b: a", DisplayErrorStack(&b).to_string());
        assert_eq!("c: b: a", DisplayErrorStack(&c).to_string());
    }
}
