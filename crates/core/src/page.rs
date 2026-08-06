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
    /// `offset` / `limit` の範囲を検証し、切り出しへ渡せる要求を返す。
    ///
    /// `offset` は符号なしのため下限側の検証は不要で、総件数を超える値は
    /// 空ページとして扱う。
    pub fn validate(&self) -> Result<ValidatedPageRequest, LimitOutOfRange> {
        if self.limit == 0 || self.limit > MAX_PAGE_LIMIT {
            return Err(LimitOutOfRange(self.limit));
        }
        Ok(ValidatedPageRequest {
            window: PageWindow {
                offset: self.offset,
                limit: self.limit,
            },
            snapshot_revision: self.snapshot_revision,
        })
    }
}

/// 検証を通ったページ要求。
///
/// 作れるのは [`PageRequest::validate`] だけである。[`take_page`] はこの型しか
/// 受け取らないため、範囲を検証していない要求で切り出すことはできない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedPageRequest {
    window: PageWindow,
    snapshot_revision: Option<u64>,
}

impl ValidatedPageRequest {
    /// revision の照合指定を落とした取り出し範囲を返す。
    ///
    /// 照合しない一覧はこの形で切り出す。落とすかどうかは一覧ごとの設計判断で
    /// あり、判断した側が [`take_window`] を呼ぶことで失敗の種類が 0 になる。
    pub fn window(&self) -> PageWindow {
        self.window
    }
}

/// 検証を通った要求は、要求そのものの表現へ戻せる。
///
/// 要求を運ぶ経路は JSON へ直列化するため、ワイヤ上の形は [`PageRequest`] の
/// ままである。[`PageRequest`] 自体はこの変換を通さずにも作れる——検証を通した
/// 結果からこの形へ戻すのは、要求を組み立てる側が守る規律である。
impl From<ValidatedPageRequest> for PageRequest {
    fn from(request: ValidatedPageRequest) -> Self {
        Self {
            offset: request.window.offset,
            limit: request.window.limit,
            snapshot_revision: request.snapshot_revision,
        }
    }
}

/// 検証を通った取り出し範囲。
///
/// revision の照合指定を持たないため、[`take_window`] は失敗しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageWindow {
    offset: u32,
    limit: u32,
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
    ///
    /// **`next_offset.is_some()` の再掲であり、値は導出できる。** 反復の終端を
    /// 判定する最も素直な口として、導出させずに載せる。生成口は [`take_page`]
    /// 1 つであり、`next_offset` と食い違う組は作られない。
    pub has_more: bool,
    /// 次のオフセット。無ければ null。
    pub next_offset: Option<u32>,
    /// このページを切り出したスナップショットの revision。
    pub snapshot_revision: u64,
}

/// `limit` が 1..=[`MAX_PAGE_LIMIT`] の外。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("limit は 1 以上 {MAX_PAGE_LIMIT} 以下である必要があります: {0}")]
pub struct LimitOutOfRange(pub u32);

/// 要求が指定した snapshot revision と現在のスナップショットが不一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("snapshot revision が一致しません: 要求 {requested}, 現在 {current}")]
pub struct SnapshotRevisionMismatch {
    /// 要求が指定した revision。
    pub requested: u64,
    /// スナップショット作成時点の revision。
    pub current: u64,
}

/// スナップショットから要求ページを切り出し、[`PageMeta`] を組み立てる。
///
/// `snapshot_revision` は `items` を列挙した時点の project revision である。
/// 要求が revision を指定していて一致しない場合は、先頭からの再取得が必要な
/// 状態であるため [`SnapshotRevisionMismatch`] を返す。失敗はこの 1 種類だけ
/// である——`limit` の範囲は要求の型が既に保証している。
pub fn take_page<T: Clone>(
    items: &[T],
    request: &ValidatedPageRequest,
    snapshot_revision: u64,
) -> Result<(Vec<T>, PageMeta), SnapshotRevisionMismatch> {
    if let Some(requested) = request.snapshot_revision
        && requested != snapshot_revision
    {
        return Err(SnapshotRevisionMismatch {
            requested,
            current: snapshot_revision,
        });
    }

    Ok(take_window(items, &request.window, snapshot_revision))
}

/// スナップショットから取り出し範囲を切り出し、[`PageMeta`] を組み立てる。
///
/// 照合する revision を持たないため失敗しない。応答へ載せる
/// `snapshot_revision` は照合とは別の値であり、`items` を列挙した時点の
/// project revision をそのまま伝える。
pub fn take_window<T: Clone>(
    items: &[T],
    window: &PageWindow,
    snapshot_revision: u64,
) -> (Vec<T>, PageMeta) {
    let total = items.len();
    let offset = window.offset as usize;
    let page_end = offset.saturating_add(window.limit as usize);
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
        offset: window.offset,
        has_more: next_offset.is_some(),
        next_offset,
        snapshot_revision,
    };
    (page, meta)
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

    fn validated(offset: u32, limit: u32, snapshot_revision: Option<u64>) -> ValidatedPageRequest {
        PageRequest {
            offset,
            limit,
            snapshot_revision,
        }
        .validate()
        .expect("検証を通らないページ要求です")
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
            assert_eq!(request.validate(), Err(LimitOutOfRange(limit)));
        }
    }

    #[test]
    fn page_request_accepts_limit_bounds() {
        for limit in [1, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT] {
            let request = PageRequest {
                limit,
                ..PageRequest::default()
            };
            let validated = request.validate().expect("範囲内の limit が拒否されました");
            assert_eq!(PageRequest::from(validated), request);
        }
    }

    #[test]
    fn take_page_returns_first_page() {
        let items = sample_items(10);
        let (page, meta) = take_page(&items, &validated(0, 3, None), 42).unwrap();
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
        let (page, meta) = take_page(&items, &validated(8, 5, None), 1).unwrap();
        assert_eq!(page, vec![8, 9]);
        assert_eq!(meta.count, 2);
        assert!(!meta.has_more);
        assert_eq!(meta.next_offset, None);
    }

    #[test]
    fn take_page_beyond_total_is_empty() {
        let items = sample_items(3);
        let (page, meta) = take_page(&items, &validated(100, 10, None), 1).unwrap();
        assert!(page.is_empty());
        assert_eq!(meta.total_count, 3);
        assert_eq!(meta.count, 0);
        assert_eq!(meta.offset, 100);
        assert!(!meta.has_more);
    }

    #[test]
    fn take_page_accepts_matching_snapshot_revision() {
        let items = sample_items(3);
        let (_, meta) = take_page(&items, &validated(0, 2, Some(9)), 9).unwrap();
        assert_eq!(meta.snapshot_revision, 9);
    }

    #[test]
    fn a_window_drops_the_requested_snapshot_revision() {
        let items = sample_items(3);
        let request = validated(0, 2, Some(9));
        let (page, meta) = take_window(&items, &request.window(), 10);
        assert_eq!(page, vec![0, 1]);
        assert_eq!(meta.snapshot_revision, 10);
    }

    #[test]
    fn take_page_rejects_snapshot_revision_mismatch() {
        let items = sample_items(3);
        assert_eq!(
            take_page(&items, &validated(0, 2, Some(9)), 10),
            Err(SnapshotRevisionMismatch {
                requested: 9,
                current: 10,
            })
        );
    }

    #[test]
    fn page_meta_allows_unknown_optional_fields() {
        let s = r#"{"total_count":1,"count":1,"offset":0,"has_more":false,"next_offset":null,"snapshot_revision":3,"future":1}"#;
        let meta: PageMeta = serde_json::from_str(s).unwrap();
        assert_eq!(meta.total_count, 1);
    }
}
