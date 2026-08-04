//! `list_instances` の実装。
//!
//! MCP SDK 未使用。内部関数または CLI 経由で呼び出す。

use crate::discovery::{DiscoveryConfig, find_instances};
use aviutl2_mcp_core::{DEFAULT_PAGE_LIMIT, ErrorCode, InstanceInfo, PageRequest, take_page};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 一覧はスナップショット revision を持たないため、ページ切り出しへ渡す固定値。
///
/// 要求側も revision を指定できないため、この値が応答へ現れることはない。
const NO_SNAPSHOT_REVISION: u64 = 0;

/// `list_instances` 要求。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListInstancesRequest {
    /// 開始位置。
    #[serde(default)]
    pub offset: u32,
    /// 取得件数（1〜200、既定 50）。
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    DEFAULT_PAGE_LIMIT
}

/// 既定値は省略時の JSON 逆直列化結果と一致する。
///
/// `limit` は 0 が常に範囲外であるため、derive による 0 埋めの既定値を持たせない。
impl Default for ListInstancesRequest {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: default_limit(),
        }
    }
}

impl ListInstancesRequest {
    /// 共通のページ要求へ変換する。
    fn page(&self) -> PageRequest {
        PageRequest {
            offset: self.offset,
            limit: self.limit,
            snapshot_revision: None,
        }
    }
}

/// `list_instances` 応答。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListInstancesResponse {
    /// ページング後のインスタンス一覧。
    pub instances: Vec<InstanceInfo>,
    /// 生存確認済みの総件数。
    pub total_count: u32,
    /// 返却件数。
    pub count: u32,
    /// 要求オフセット。
    pub offset: u32,
    /// さらに次のページがあるか。
    pub has_more: bool,
    /// 次のオフセット（なければ null）。
    pub next_offset: Option<u32>,
}

/// `list_instances` の失敗。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListInstancesError {
    /// offset/limit が範囲外。
    #[error("offset または limit が範囲外です")]
    InvalidArgument,
    /// registry ディレクトリ自体を読み取れなかった。
    ///
    /// インスタンスが 0 件である場合と区別するため、正常な空結果にはしない。
    /// 原因種別のみを保持し、パスなどの詳細は応答へ載せない。
    #[error("インスタンス登録情報を読み取れませんでした: {0:?}")]
    RegistryUnreadable(std::io::ErrorKind),
}

impl ListInstancesError {
    /// 応答へ載せるエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            ListInstancesError::InvalidArgument => ErrorCode::InvalidArgument,
            // 呼び出し側の入力に起因しない、server 環境側の想定外失敗。
            ListInstancesError::RegistryUnreadable(_) => ErrorCode::InternalError,
        }
    }
}

/// `list_instances` を実行する。
///
/// `registry_dir` が存在しない場合はインスタンス 0 件として空の結果を返す。
/// ディレクトリを列挙できない場合は 0 件と区別するためエラーを返す。
pub fn list_instances(
    registry_dir: &Path,
    request: ListInstancesRequest,
) -> Result<ListInstancesResponse, ListInstancesError> {
    // 範囲外の要求で registry を走査しないよう、生存確認の前に検証する。
    let page_request = request.page();
    page_request
        .validate()
        .map_err(|_| ListInstancesError::InvalidArgument)?;

    let all = find_instances(registry_dir, DiscoveryConfig::default(), true)
        .map_err(|e| ListInstancesError::RegistryUnreadable(e.io_error_kind()))?;

    let (instances, page) = take_page(&all, &page_request, NO_SNAPSHOT_REVISION)
        .map_err(|_| ListInstancesError::InvalidArgument)?;

    Ok(ListInstancesResponse {
        instances,
        total_count: page.total_count,
        count: page.count,
        offset: page.offset,
        has_more: page.has_more,
        next_offset: page.next_offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::{
        AuthSecret, DescriptorProject, InstanceDescriptor, InstanceId, InstanceState,
        MAX_PAGE_LIMIT, ProtocolVersion, format_utc_timestamp, pipe_name_for,
    };
    use chrono::{TimeZone, Utc};

    /// descriptor の時刻フィールドに使う固定値。
    ///
    /// 書き手と同じヘルパーを通して整形し、時刻の正準書式から外れた値を
    /// テスト固定値として持ち込まない。
    fn fixed_timestamp() -> String {
        let value = Utc
            .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
            .single()
            .expect("UTC の固定日時は一意に定まる");
        format_utc_timestamp(value)
    }

    fn temp_registry_dir() -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aviutl2-mcp-api-test-{}", InstanceId::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn descriptor_for(id: InstanceId, pid: u32) -> InstanceDescriptor {
        InstanceDescriptor {
            schema_version: 1,
            protocol_version: ProtocolVersion::CURRENT,
            instance_id: id,
            pipe_name: pipe_name_for(&id),
            auth_secret: AuthSecret::generate(),
            pid,
            process_created_at: fixed_timestamp(),
            hwnd: None,
            started_at: fixed_timestamp(),
            state: InstanceState::Ready,
            project: Some(DescriptorProject {
                display_name: "Test".to_string(),
                path: r"C:\test.aup".to_string(),
            }),
        }
    }

    #[test]
    fn empty_registry_returns_empty() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let response = list_instances(
            &dir,
            ListInstancesRequest {
                offset: 0,
                limit: 50,
            },
        )
        .unwrap();
        assert_eq!(response.total_count, 0);
        assert_eq!(response.count, 0);
        assert!(!response.has_more);
        assert!(response.next_offset.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_request_uses_default_limit() {
        let request = ListInstancesRequest::default();
        assert_eq!(request.offset, 0);
        assert_eq!(request.limit, DEFAULT_PAGE_LIMIT);
        assert_eq!(request.limit, 50);
        assert_eq!(
            request.page().validate(),
            Ok(()),
            "既定値はそのまま受理される"
        );
    }

    #[test]
    fn omitted_fields_match_default() {
        let request: ListInstancesRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(request.offset, ListInstancesRequest::default().offset);
        assert_eq!(request.limit, ListInstancesRequest::default().limit);
    }

    #[test]
    fn wire_form_keeps_offset_and_limit_flat() {
        let value = serde_json::to_value(ListInstancesRequest {
            offset: 10,
            limit: 25,
        })
        .unwrap();
        assert_eq!(value, serde_json::json!({ "offset": 10, "limit": 25 }));
    }

    #[test]
    fn unknown_field_rejected() {
        assert!(
            serde_json::from_str::<ListInstancesRequest>(r#"{"snapshot_revision":1}"#).is_err()
        );
    }

    #[test]
    fn invalid_limit_rejected() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            list_instances(
                &dir,
                ListInstancesRequest {
                    offset: 0,
                    limit: 0,
                }
            )
            .is_err()
        );
        assert!(
            list_instances(
                &dir,
                ListInstancesRequest {
                    offset: 0,
                    limit: 201,
                }
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_registry_dir_returns_zero_instances() {
        let dir = temp_registry_dir();
        assert!(!dir.exists());

        let response = list_instances(
            &dir,
            ListInstancesRequest {
                offset: 0,
                limit: 50,
            },
        )
        .expect("ディレクトリ不在は 0 件の正常応答");
        assert_eq!(response.total_count, 0);
        assert_eq!(response.count, 0);
    }

    #[test]
    fn unreadable_registry_dir_returns_error() {
        // ディレクトリとして開けない対象を registry として渡す。
        let path = std::env::temp_dir().join(format!(
            "aviutl2-mcp-api-not-a-dir-{}",
            InstanceId::new_v4()
        ));
        std::fs::write(&path, b"not a directory").unwrap();

        let error = list_instances(
            &path,
            ListInstancesRequest {
                offset: 0,
                limit: 50,
            },
        )
        .expect_err("読み取り失敗は 0 件と区別してエラーにする");
        assert!(
            matches!(error, ListInstancesError::RegistryUnreadable(_)),
            "実際のエラー: {error:?}"
        );
        assert_ne!(
            error,
            ListInstancesError::RegistryUnreadable(std::io::ErrorKind::NotFound),
            "ディレクトリ不在は 0 件として扱われるため、この経路には現れない"
        );
        assert_eq!(error.error_code(), ErrorCode::InternalError);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn offset_beyond_total_returns_empty_page() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();

        let response = list_instances(
            &dir,
            ListInstancesRequest {
                offset: u32::MAX,
                limit: MAX_PAGE_LIMIT,
            },
        )
        .expect("総件数を超える offset は空ページを返す");
        assert_eq!(response.offset, u32::MAX);
        assert_eq!(response.total_count, 0);
        assert_eq!(response.count, 0);
        assert!(!response.has_more);
        assert!(response.next_offset.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn limit_boundaries_accepted() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();

        for limit in [1, MAX_PAGE_LIMIT] {
            assert!(
                list_instances(&dir, ListInstancesRequest { offset: 0, limit }).is_ok(),
                "limit {limit} は許容される"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_descriptor_not_listed() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        let descriptor = descriptor_for(id, 0xFFFF_FFFF);
        let path = dir.join(format!("{}.json", id));
        std::fs::write(&path, serde_json::to_string(&descriptor).unwrap()).unwrap();

        let response = list_instances(
            &dir,
            ListInstancesRequest {
                offset: 0,
                limit: 50,
            },
        )
        .unwrap();
        assert_eq!(response.total_count, 0);
        assert!(!path.exists(), "stale descriptor should be cleaned up");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_json_excluded() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        let path = dir.join(format!("{}.json", id));
        std::fs::write(&path, b"not json").unwrap();

        let response = list_instances(
            &dir,
            ListInstancesRequest {
                offset: 0,
                limit: 50,
            },
        )
        .unwrap();
        assert_eq!(response.total_count, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
