//! 一覧取得の pagination 型と適用規則。

use serde::{Deserialize, Serialize};

/// 1 ページあたりの既定件数。
pub const DEFAULT_PAGE_LIMIT: u32 = 50;

/// 1 ページあたりの最大件数。
pub const MAX_PAGE_LIMIT: u32 = 200;

/// ページ要求。
///
/// 入力型であるため未知フィールドを拒否する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageRequest {
    /// 開始位置。0 始まり。
    #[serde(default)]
    pub offset: u32,
    /// 取得件数。1 以上 [`MAX_PAGE_LIMIT`] 以下。
    #[serde(default = "default_page_limit")]
    pub limit: u32,
    /// 先頭ページが返した snapshot revision。指定時は一致を必須とする。
    #[serde(default)]
    pub snapshot_revision: Option<u64>,
}

fn default_page_limit() -> u32 {
    DEFAULT_PAGE_LIMIT
}

/// 既定値は省略時の JSON 逆直列化結果と一致する。
///
/// `limit` は 0 が常に範囲外であるため、derive による 0 埋めの既定値を持たせない。
impl Default for PageRequest {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: DEFAULT_PAGE_LIMIT,
            snapshot_revision: None,
        }
    }
}

impl PageRequest {
    /// `offset` / `limit` の範囲を検証する。
    ///
    /// `offset` は符号なしのため下限側の検証は不要で、総件数を超える値は
    /// 空ページとして扱う。
    pub fn validate(&self) -> Result<(), PageError> {
        if self.limit == 0 || self.limit > MAX_PAGE_LIMIT {
            return Err(PageError::LimitOutOfRange(self.limit));
        }
        Ok(())
    }
}

/// ページ応答のメタ情報。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageMeta {
    /// スナップショット全体の件数。
    pub total_count: u32,
    /// このページの件数。
    pub count: u32,
    /// 要求オフセット。
    pub offset: u32,
    /// さらに次のページがあるか。
    pub has_more: bool,
    /// 次のオフセット。無ければ null。
    pub next_offset: Option<u32>,
    /// このページを切り出したスナップショットの revision。
    pub snapshot_revision: u64,
}

/// pagination の検証失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PageError {
    /// `limit` が 1..=[`MAX_PAGE_LIMIT`] の外。
    #[error("limit は 1 以上 {MAX_PAGE_LIMIT} 以下である必要があります: {0}")]
    LimitOutOfRange(u32),
    /// 要求が指定した snapshot revision と現在のスナップショットが不一致。
    #[error("snapshot revision が一致しません: 要求 {requested}, 現在 {current}")]
    SnapshotRevisionMismatch {
        /// 要求が指定した revision。
        requested: u64,
        /// スナップショット作成時点の revision。
        current: u64,
    },
}

/// スナップショットから要求ページを切り出し、[`PageMeta`] を組み立てる。
///
/// `snapshot_revision` は `items` を列挙した時点の project revision である。
/// 要求が revision を指定していて一致しない場合は、先頭からの再取得が必要な
/// 状態であるため [`PageError::SnapshotRevisionMismatch`] を返す。
pub fn take_page<T: Clone>(
    items: &[T],
    request: &PageRequest,
    snapshot_revision: u64,
) -> Result<(Vec<T>, PageMeta), PageError> {
    request.validate()?;

    if let Some(requested) = request.snapshot_revision
        && requested != snapshot_revision
    {
        return Err(PageError::SnapshotRevisionMismatch {
            requested,
            current: snapshot_revision,
        });
    }

    let total = items.len();
    let offset = request.offset as usize;
    let page_end = offset.saturating_add(request.limit as usize);
    let page = if offset >= total {
        Vec::new()
    } else {
        items[offset..page_end.min(total)].to_vec()
    };

    let next_offset = if page_end < total {
        Some(saturating_u32(page_end))
    } else {
        None
    };

    let meta = PageMeta {
        total_count: saturating_u32(total),
        count: saturating_u32(page.len()),
        offset: request.offset,
        has_more: next_offset.is_some(),
        next_offset,
        snapshot_revision,
    };
    Ok((page, meta))
}

/// 件数を `u32` へ落とす。上限を超える件数は飽和させる。
fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_items(count: usize) -> Vec<u32> {
        (0..count as u32).collect()
    }

    #[test]
    fn page_request_default_uses_default_limit() {
        let request = PageRequest::default();
        assert_eq!(request.limit, DEFAULT_PAGE_LIMIT);
        assert_eq!(request.limit, 50);
        assert_eq!(request.offset, 0);
        assert_eq!(request.snapshot_revision, None);
    }

    #[test]
    fn page_request_omitted_fields_match_default() {
        let request: PageRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(request, PageRequest::default());
    }

    #[test]
    fn page_request_roundtrip() {
        let request = PageRequest {
            offset: 10,
            limit: 25,
            snapshot_revision: Some(7),
        };
        let s = serde_json::to_string(&request).unwrap();
        let restored: PageRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, request);
    }

    #[test]
    fn page_request_rejects_unknown_field() {
        let result: Result<PageRequest, _> = serde_json::from_str(r#"{"offset":0,"future":1}"#);
        assert!(result.is_err());
    }

    #[test]
    fn page_request_rejects_limit_out_of_range() {
        for limit in [0, MAX_PAGE_LIMIT + 1, u32::MAX] {
            let request = PageRequest {
                limit,
                ..PageRequest::default()
            };
            assert_eq!(request.validate(), Err(PageError::LimitOutOfRange(limit)));
        }
    }

    #[test]
    fn page_request_accepts_limit_bounds() {
        for limit in [1, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT] {
            let request = PageRequest {
                limit,
                ..PageRequest::default()
            };
            assert_eq!(request.validate(), Ok(()));
        }
    }

    #[test]
    fn take_page_returns_first_page() {
        let items = sample_items(10);
        let request = PageRequest {
            offset: 0,
            limit: 3,
            snapshot_revision: None,
        };
        let (page, meta) = take_page(&items, &request, 42).unwrap();
        assert_eq!(page, vec![0, 1, 2]);
        assert_eq!(
            meta,
            PageMeta {
                total_count: 10,
                count: 3,
                offset: 0,
                has_more: true,
                next_offset: Some(3),
                snapshot_revision: 42,
            }
        );
    }

    #[test]
    fn take_page_last_page_has_no_next_offset() {
        let items = sample_items(10);
        let request = PageRequest {
            offset: 8,
            limit: 5,
            snapshot_revision: None,
        };
        let (page, meta) = take_page(&items, &request, 1).unwrap();
        assert_eq!(page, vec![8, 9]);
        assert_eq!(meta.count, 2);
        assert!(!meta.has_more);
        assert_eq!(meta.next_offset, None);
    }

    #[test]
    fn take_page_beyond_total_is_empty() {
        let items = sample_items(3);
        let request = PageRequest {
            offset: 100,
            limit: 10,
            snapshot_revision: None,
        };
        let (page, meta) = take_page(&items, &request, 1).unwrap();
        assert!(page.is_empty());
        assert_eq!(meta.total_count, 3);
        assert_eq!(meta.count, 0);
        assert_eq!(meta.offset, 100);
        assert!(!meta.has_more);
    }

    #[test]
    fn take_page_accepts_matching_snapshot_revision() {
        let items = sample_items(3);
        let request = PageRequest {
            offset: 0,
            limit: 2,
            snapshot_revision: Some(9),
        };
        let (_, meta) = take_page(&items, &request, 9).unwrap();
        assert_eq!(meta.snapshot_revision, 9);
    }

    #[test]
    fn take_page_rejects_snapshot_revision_mismatch() {
        let items = sample_items(3);
        let request = PageRequest {
            offset: 0,
            limit: 2,
            snapshot_revision: Some(9),
        };
        assert_eq!(
            take_page(&items, &request, 10),
            Err(PageError::SnapshotRevisionMismatch {
                requested: 9,
                current: 10,
            })
        );
    }

    #[test]
    fn take_page_rejects_invalid_limit() {
        let items = sample_items(3);
        let request = PageRequest {
            offset: 0,
            limit: 0,
            snapshot_revision: None,
        };
        assert_eq!(
            take_page(&items, &request, 1),
            Err(PageError::LimitOutOfRange(0))
        );
    }

    #[test]
    fn page_meta_allows_unknown_optional_fields() {
        let s = r#"{"total_count":1,"count":1,"offset":0,"has_more":false,"next_offset":null,"snapshot_revision":3,"future":1}"#;
        let meta: PageMeta = serde_json::from_str(s).unwrap();
        assert_eq!(meta.total_count, 1);
    }
}
