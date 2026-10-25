use std::error::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Info(String),
    Failure(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeRaised(pub Notice);

pub fn error_chain(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();

    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }

    text
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use super::*;

    #[derive(Debug)]
    struct Outer(Inner);

    #[derive(Debug)]
    struct Inner;

    impl fmt::Display for Outer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("outer")
        }
    }

    impl fmt::Display for Inner {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("inner")
        }
    }

    impl Error for Outer {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }

    impl Error for Inner {}

    #[test]
    fn chain_joins_every_cause() {
        assert_eq!(error_chain(&Outer(Inner)), "outer: inner");
    }
}
