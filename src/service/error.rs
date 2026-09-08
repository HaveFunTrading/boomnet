//! Error types produced by the I/O service lifecycle.

use crate::service::Handle;
use crate::service::endpoint::DisconnectReason;
use std::fmt::{Display, Formatter};
use std::io;

/// Operation being performed when an [`IOServiceError::IO`] occurred.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IOServiceOperation {
    /// Building a factory through [`crate::service::IOService::register_with`].
    CreateFactory,
    /// Resolving an endpoint address.
    Resolve,
    /// Creating an endpoint through a registered factory.
    CreateEndpoint,
    /// Polling the configured selector.
    PollSelector,
    /// Registering a connected endpoint with the selector.
    Register,
    /// Unregistering an endpoint from the selector.
    Unregister,
}

impl Display for IOServiceOperation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateFactory => f.write_str("create endpoint factory"),
            Self::Resolve => f.write_str("resolve endpoint"),
            Self::CreateEndpoint => f.write_str("create endpoint"),
            Self::PollSelector => f.write_str("poll selector"),
            Self::Register => f.write_str("register endpoint"),
            Self::Unregister => f.write_str("unregister endpoint"),
        }
    }
}

/// Failure produced while advancing an I/O service.
#[derive(Debug)]
pub enum IOServiceError {
    /// An underlying I/O operation failed.
    IO {
        /// Registration associated with the failure, when applicable.
        handle: Option<Handle>,
        /// Operation that failed.
        operation: IOServiceOperation,
        /// Underlying I/O failure.
        source: io::Error,
    },
    /// An endpoint disconnected and its factory declined recreation.
    EndpointNotRecreatable {
        /// Terminal endpoint handle.
        handle: Handle,
        /// Reason the endpoint disconnected.
        reason: DisconnectReason,
    },
    /// Internal service state was inconsistent.
    InvalidState {
        /// Registration associated with the invalid state, when applicable.
        handle: Option<Handle>,
        /// Description of the violated invariant.
        message: &'static str,
    },
}

impl Display for IOServiceError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IO {
                handle,
                operation,
                source,
            } => match handle {
                Some(handle) => write!(f, "failed to {operation} for endpoint {handle:?}: {source}"),
                None => write!(f, "failed to {operation}: {source}"),
            },
            Self::EndpointNotRecreatable { handle, reason } => {
                write!(f, "endpoint {handle:?} cannot be recreated after {reason}")
            }
            Self::InvalidState { handle, message } => match handle {
                Some(handle) => write!(f, "invalid state for endpoint {handle:?}: {message}"),
                None => write!(f, "invalid I/O service state: {message}"),
            },
        }
    }
}

impl std::error::Error for IOServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IO { source, .. } => Some(source),
            Self::EndpointNotRecreatable {
                reason: DisconnectReason::IO(source),
                ..
            } => Some(source),
            Self::EndpointNotRecreatable { .. } | Self::InvalidState { .. } => None,
        }
    }
}

impl IOServiceError {
    pub(crate) fn io(handle: Option<Handle>, operation: IOServiceOperation, source: io::Error) -> Self {
        Self::IO {
            handle,
            operation,
            source,
        }
    }
}
