use std::fmt::Debug;

pub trait ResultExt {
    fn trace_err(self) -> Self;
}

impl<T, E: Debug> ResultExt for Result<T, E> {
    fn trace_err(self) -> Self {
        if let Err(e) = &self {
            tracing::error!("{e:#?}");
        }
        self
    }
}
