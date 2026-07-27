//! `aviutl2_list_instances` の実装。
//!
//! MCP SDK 未使用。内部関数または CLI 経由で呼び出す。

use crate::discovery::{DiscoveryConfig, find_instances};
use aviutl2_mcp_core::InstanceInfo;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 1 ページあたりの既定件数。
const DEFAULT_LIMIT: u32 = 50;
/// 最大件数。
const MAX_LIMIT: u32 = 200;

/// `aviutl2_list_instances` 要求。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
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
    DEFAULT_LIMIT
}

/// `aviutl2_list_instances` 応答。
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

/// 引数検証エラー。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListInstancesError {
    /// offset/limit が範囲外。
    #[error("offset または limit が範囲外です")]
    InvalidArgument,
}

/// `aviutl2_list_instances` を実行する。
///
/// `registry_dir` が存在しない場合は空の結果を返す。
pub fn aviutl2_list_instances(
    registry_dir: &Path,
    request: ListInstancesRequest,
) -> Result<ListInstancesResponse, ListInstancesError> {
    if request.offset > i64::MAX as u32 {
        return Err(ListInstancesError::InvalidArgument);
    }
    if request.limit == 0 || request.limit > MAX_LIMIT {
        return Err(ListInstancesError::InvalidArgument);
    }

    let all = find_instances(registry_dir, DiscoveryConfig::default(), true);
    let total_count = all.len() as u32;
    let offset = request.offset as usize;
    let limit = request.limit as usize;

    let page = if offset >= all.len() {
        Vec::new()
    } else {
        let end = (offset + limit).min(all.len());
        all[offset..end].to_vec()
    };

    let count = page.len() as u32;
    let next_offset = if offset + limit < all.len() {
        Some((offset + limit) as u32)
    } else {
        None
    };

    Ok(ListInstancesResponse {
        instances: page,
        total_count,
        count,
        offset: request.offset,
        has_more: next_offset.is_some(),
        next_offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::{
        AuthSecret, DescriptorProject, InstanceDescriptor, InstanceId, InstanceState,
        ProtocolVersion, pipe_name_for,
    };

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
            process_created_at: "2026-01-01T00:00:00Z".to_string(),
            hwnd: None,
            started_at: "2026-01-01T00:00:00Z".to_string(),
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
        let response = aviutl2_list_instances(
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
    fn invalid_limit_rejected() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            aviutl2_list_instances(
                &dir,
                ListInstancesRequest {
                    offset: 0,
                    limit: 0,
                }
            )
            .is_err()
        );
        assert!(
            aviutl2_list_instances(
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
    fn stale_descriptor_not_listed() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        let descriptor = descriptor_for(id, 0xFFFF_FFFF);
        let path = dir.join(format!("{}.json", id));
        std::fs::write(&path, serde_json::to_string(&descriptor).unwrap()).unwrap();

        let response = aviutl2_list_instances(
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

        let response = aviutl2_list_instances(
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
