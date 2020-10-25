// TODO: Display impls

/// Error type for `Consumer::try_pop()`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PopError {
    /// The queue was empty.
    Empty,
}

/// Error type for `Producer::try_push()`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PushError<T> {
    /// The queue was full.
    Full(T),
}
