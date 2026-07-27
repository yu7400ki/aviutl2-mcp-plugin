//! プロトコルバージョン negotiation。

use crate::error::ErrorCode;
use crate::identifier::ProtocolVersion;

/// ローカルとリモートの最大プロトコルバージョンを交渉し、採用版を返す。
///
/// - MAJOR 不一致の場合は `protocol_mismatch` エラー。
/// - 同一 MAJOR の場合は両者の MINOR の小さい方を採用する。
pub fn negotiate(
    local_max: ProtocolVersion,
    remote_max: ProtocolVersion,
) -> Result<ProtocolVersion, ErrorCode> {
    if local_max.major != remote_max.major {
        return Err(ErrorCode::ProtocolMismatch);
    }
    Ok(ProtocolVersion {
        major: local_max.major,
        minor: local_max.minor.min(remote_max.minor),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_major_min_minor() {
        let local = ProtocolVersion { major: 1, minor: 2 };
        let remote = ProtocolVersion { major: 1, minor: 5 };
        assert_eq!(
            negotiate(local, remote).unwrap(),
            ProtocolVersion { major: 1, minor: 2 }
        );
    }

    #[test]
    fn same_major_local_higher() {
        let local = ProtocolVersion { major: 1, minor: 5 };
        let remote = ProtocolVersion { major: 1, minor: 2 };
        assert_eq!(
            negotiate(local, remote).unwrap(),
            ProtocolVersion { major: 1, minor: 2 }
        );
    }

    #[test]
    fn major_mismatch_returns_error() {
        let local = ProtocolVersion { major: 1, minor: 0 };
        let remote = ProtocolVersion { major: 2, minor: 0 };
        assert_eq!(
            negotiate(local, remote).unwrap_err(),
            ErrorCode::ProtocolMismatch
        );
    }

    #[test]
    fn same_version() {
        let v = ProtocolVersion { major: 1, minor: 0 };
        assert_eq!(negotiate(v, v).unwrap(), v);
    }
}
