use std::fmt;

use crate::{driver::Phase, sys};

/// A driver return code or a failure of the owner/query contract.
#[derive(Debug)]
pub enum Error {
    Com(windows_core::Error),
    /// Includes unknown codes and zero when an ASIO future rejected a request.
    Driver {
        operation: &'static str,
        code: sys::ResultCode,
    },
    InitializationRejected,
    InvalidState {
        operation: &'static str,
        phase: Phase,
    },
    InvalidArgument {
        field: &'static str,
    },
    InvalidResponse {
        operation: &'static str,
        detail: &'static str,
    },
    InvalidBoolean {
        field: &'static str,
        value: i32,
    },
    UnterminatedString {
        field: &'static str,
    },
    CapacityExceeded {
        operation: &'static str,
        capacity: usize,
        reported: i32,
    },
    Allocation,
}

impl Error {
    /// The original ASIO return value, including zero; local/COM errors have none.
    pub const fn code(&self) -> Option<sys::ResultCode> {
        match self {
            Self::Driver { code, .. } => Some(*code),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Com(error) => write!(f, "COM: {error}"),
            Self::Driver { operation, code } => write!(f, "{operation}: {code:?}"),
            Self::InitializationRejected => f.write_str("driver rejected initialization"),
            Self::InvalidState { operation, phase } => {
                write!(f, "{operation} is invalid in phase {phase:?}")
            }
            Self::InvalidArgument { field } => write!(f, "invalid argument: {field}"),
            Self::InvalidResponse { operation, detail } => write!(f, "{operation}: {detail}"),
            Self::InvalidBoolean { field, value } => write!(f, "{field}: invalid ASIOBool {value}"),
            Self::UnterminatedString { field } => write!(f, "{field}: missing NUL terminator"),
            Self::CapacityExceeded {
                operation,
                capacity,
                reported,
            } => {
                write!(
                    f,
                    "{operation}: reported {reported} entries for capacity {capacity}"
                )
            }
            Self::Allocation => f.write_str("control-side allocation failed"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Com(error) => Some(error),
            _ => None,
        }
    }
}

impl From<windows_core::Error> for Error {
    fn from(error: windows_core::Error) -> Self {
        Self::Com(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
