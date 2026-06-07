//! The domain error model (§21). Maps to gRPC status codes and FFI status.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    #[error("timeout after {0} ms")]
    Timeout(u64),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("terminal crashed: {0}")]
    TerminalCrashed(String),
    #[error("bad argument: {0}")]
    BadArgument(String),
    #[error("protocol mismatch: {0}")]
    ProtocolMismatch(String),
    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// gRPC status code names (string form so muse-core stays I/O-free).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrpcCode {
    Internal,
    DeadlineExceeded,
    NotFound,
    Aborted,
    InvalidArgument,
    FailedPrecondition,
}

/// FFI status enum, mirrored by the C ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum FfiStatus {
    Ok = 0,
    Timeout = 1,
    NotFound = 2,
    Crashed = 3,
    BadArg = 4,
    Internal = 5,
}

impl Error {
    pub fn grpc_code(&self) -> GrpcCode {
        match self {
            Error::SpawnFailed(_) => GrpcCode::Internal,
            Error::Timeout(_) => GrpcCode::DeadlineExceeded,
            Error::NotFound(_) => GrpcCode::NotFound,
            Error::TerminalCrashed(_) => GrpcCode::Aborted,
            Error::BadArgument(_) => GrpcCode::InvalidArgument,
            Error::ProtocolMismatch(_) => GrpcCode::FailedPrecondition,
            Error::Internal(_) => GrpcCode::Internal,
        }
    }

    pub fn ffi_status(&self) -> FfiStatus {
        match self {
            Error::SpawnFailed(_) => FfiStatus::Internal,
            Error::Timeout(_) => FfiStatus::Timeout,
            Error::NotFound(_) => FfiStatus::NotFound,
            Error::TerminalCrashed(_) => FfiStatus::Crashed,
            Error::BadArgument(_) => FfiStatus::BadArg,
            Error::ProtocolMismatch(_) => FfiStatus::BadArg,
            Error::Internal(_) => FfiStatus::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_mapping_complete() {
        assert_eq!(
            Error::SpawnFailed("x".into()).grpc_code(),
            GrpcCode::Internal
        );
        assert_eq!(Error::Timeout(1).grpc_code(), GrpcCode::DeadlineExceeded);
        assert_eq!(Error::NotFound("x".into()).grpc_code(), GrpcCode::NotFound);
        assert_eq!(
            Error::TerminalCrashed("x".into()).grpc_code(),
            GrpcCode::Aborted
        );
        assert_eq!(
            Error::BadArgument("x".into()).grpc_code(),
            GrpcCode::InvalidArgument
        );
        assert_eq!(
            Error::ProtocolMismatch("x".into()).grpc_code(),
            GrpcCode::FailedPrecondition
        );
        assert_eq!(Error::Internal("x".into()).grpc_code(), GrpcCode::Internal);
    }

    #[test]
    fn ffi_mapping_complete() {
        assert_eq!(
            Error::SpawnFailed("x".into()).ffi_status(),
            FfiStatus::Internal
        );
        assert_eq!(Error::Timeout(1).ffi_status(), FfiStatus::Timeout);
        assert_eq!(
            Error::NotFound("x".into()).ffi_status(),
            FfiStatus::NotFound
        );
        assert_eq!(
            Error::TerminalCrashed("x".into()).ffi_status(),
            FfiStatus::Crashed
        );
        assert_eq!(
            Error::BadArgument("x".into()).ffi_status(),
            FfiStatus::BadArg
        );
        assert_eq!(
            Error::ProtocolMismatch("x".into()).ffi_status(),
            FfiStatus::BadArg
        );
        assert_eq!(
            Error::Internal("x".into()).ffi_status(),
            FfiStatus::Internal
        );
    }

    #[test]
    fn display_messages() {
        assert_eq!(Error::Timeout(50).to_string(), "timeout after 50 ms");
        assert!(Error::SpawnFailed("boom".into())
            .to_string()
            .contains("boom"));
    }

    #[test]
    fn ffi_status_repr() {
        assert_eq!(FfiStatus::Ok as i32, 0);
        assert_eq!(FfiStatus::Internal as i32, 5);
    }
}
