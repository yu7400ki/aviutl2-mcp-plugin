//! discovery / IPC の失敗を tool result のエラーへ変換する。
//!
//! SDK・IPC の失敗は MCP の protocol error にせず、`isError: true` の tool result
//! として返す。呼び出し側が読める説明を text に、機械可読な `code` /`retryable` /
//! `details` / `correlation_id` を `structuredContent` に載せる。

use crate::discovery::ResolveInstanceError;
use crate::mcp::summary::clamp_chars;
use crate::pipe_client::PipeClientError;
use aviutl2_mcp_core::{EditOperation, ErrorCode, ErrorObject};
use serde_json::{Map, Value};

/// `details` から取り除く key の断片（小文字で比較する）。
///
/// 秘匿値・生ポインタ・内部ハンドル・絶対パスは応答へ出さない。key 名の完全一致では
/// 将来の命名を取りこぼすため、断片を含むかで判定する。
///
/// これは失敗の `details` という自由な形の値に対する保守的な既定であり、成功応答の
/// DTO が何を公開するかとは別物である。`alias` や `path` を名前に含む値でも、DTO が
/// 意図して公開しているものはそのまま返る。逆に、ここへ断片が加わると同名の
/// `details` は黙って落ちるため、正当な診断値を載せる場合は断片との衝突を確かめる。
const SENSITIVE_KEY_FRAGMENTS: &[&str] = &[
    "secret",
    "nonce",
    "mac",
    "token",
    "password",
    "credential",
    "pipe",
    "handle",
    "hwnd",
    "pointer",
    "ptr",
    "address",
    "alias",
    "path",
];

/// `details` の文字列値に許す最大文字数。
///
/// 接続先が名前を切り詰める長さと揃える。`details` に残る文字列は effect 名・
/// 設定項目名・オブジェクト名のように呼び出し側が要求を訂正するのに使う識別子で
/// あり、ここでさらに短く切ると、長い名前が識別できないまま返る。
///
/// **接続先が全ての文字列を抑えているわけではない。** 接続先が切り詰めるのは
/// 要求が名指しした effect 名・設定項目名だけであり、読み直した対象の概要が運ぶ
/// オブジェクト名はそのまま届く。上限を確実に掛けているのはこの一点である。
const MAX_DETAIL_STRING_CHARS: usize = 1_024;

/// `details` の配列に残す最大要素数。
const MAX_DETAIL_ARRAY_ITEMS: usize = 32;

/// `details` を辿る最大の深さ。
const MAX_DETAIL_DEPTH: usize = 8;

/// エラーメッセージに許す最大文字数。
const MAX_MESSAGE_CHARS: usize = 400;

/// text content へ出す `details` の行の合計に許す最大文字数。
///
/// `sanitize_details` は要素ごとの上限（文字列・配列の長さ、深さ）を持つが、
/// key の数に上限が無いため合計が決まらない。ここで合計を止める。
///
/// [`MAX_MESSAGE_CHARS`] と同じく要求元が動かせない。要求の内容で応答の費用が
/// 変わらないようにするためである。
///
/// 値は [`MAX_DETAIL_STRING_CHARS`] の 3 倍を超える幅を取ってある。上限まで
/// 伸びた文字列値を持つ key が複数あっても落ちず、それでいて text content 全体の
/// 上限（[`crate::mcp::summary::MAX_TEXT_CHARS`]）の一部に収まる。
const MAX_DETAIL_TEXT_CHARS: usize = 4_000;

/// 入力検証の失敗を表すエラーを作る。
pub fn invalid_argument(message: impl Into<String>) -> ErrorObject {
    from_code(ErrorCode::InvalidArgument, message)
}

/// server 内部の想定外失敗を表すエラーを作る。
pub fn internal_error(message: impl Into<String>) -> ErrorObject {
    from_code(ErrorCode::InternalError, message)
}

/// コードと説明からエラーを作る。`retryable` はコードの既定値を用いる。
pub fn from_code(code: ErrorCode, message: impl Into<String>) -> ErrorObject {
    let retryable = code.default_retryable();
    ErrorObject::new(
        code,
        clamp_chars(&message.into(), MAX_MESSAGE_CHARS),
        retryable,
    )
}

/// インスタンス解決の失敗をエラーへ変換する。
///
/// インスタンスが応答を返した場合はその [`ErrorObject`] をそのまま用い、
/// `retry_after_ms` のような待ち直しに必要な情報を落とさない。
pub fn from_resolve_error(error: &ResolveInstanceError) -> ErrorObject {
    if let Some(remote) = error.remote_error() {
        return sanitize(remote.clone());
    }
    from_code(error.error_code(), describe_resolve_error(error))
}

/// 要求送信の失敗をエラーへ変換する。
///
/// 接続先が返したエラー応答はそのまま用いる。応答を受け取れなかった失敗は
/// server 側で組み立て、編集 operation であれば変更の有無が不明であることを
/// 機械可読な補助情報として添える。
///
/// 予算切れや接続断は、接続先が編集区間へ入ったあとにも起こり得る。そのとき
/// 変更は適用され取り消し履歴にも載っているのに、要求元は `timeout` を受け取る。
/// 補助情報が無いと、要求元は「変更は入っていない」と読んで再送し、冪等でない
/// 作成や付与を重複させる。判別できない経路は必ず不明側を名乗る。
///
/// read には添えない。読み取りは副作用を持たないため、変更の有無という問いが
/// そもそも成り立たず、`refetch` の案内も意味を持たない。
pub fn from_pipe_error(error: &PipeClientError, operation: &str) -> ErrorObject {
    if let PipeClientError::Remote(remote) = error {
        return sanitize(remote.as_ref().clone());
    }
    let error = from_code(error.error_code(), describe_pipe_error(error));
    if EditOperation::from_operation_name(operation).is_none() {
        return error;
    }
    error.with_details(serde_json::json!({
        "change_applied": "unknown",
        "mutation_origin": "server",
        "retry_requires": "refetch",
    }))
}

/// エラーへ相関 ID を設定する。
pub fn with_correlation_id(error: ErrorObject, correlation_id: &str) -> ErrorObject {
    error.with_correlation_id(correlation_id)
}

/// エラーを `structuredContent` へ載せる形へ変換する。
///
/// この形は tool が宣言する `outputSchema` には適合しない。成功と失敗で構造が
/// 異なるのは MCP の tool result がそう定めているためであり、`isError` が真の
/// ときに `outputSchema` を適用してはならない。
pub fn structured(error: &ErrorObject) -> Value {
    serde_json::json!({
        "code": error.code.as_snake_case(),
        "message": error.message,
        "retryable": error.retryable,
        "details": error.details,
        "correlation_id": error.correlation_id,
    })
}

/// レイヤーのロックで拒否された要求へ添える案内。
///
/// `retry_requires` は `none` である。同じ要求の再送でも対象の読み直しでも
/// 解消しないが、**別の operation を挟めば解ける**。その手順を機械可読な分類へ
/// 押し込むと値が増えるたびに要求元の分岐が増えるため、案内は text が担う。
const LAYER_LOCKED_GUIDANCE: &str =
    "対象のレイヤーがロックされています。set_layer_state でロックを解除してから再実行してください";

/// 対象の現在の姿を返した失敗へ添える案内。
///
/// 値そのものは `details` の行が運ぶ。ここが述べるのは、その値を読み直さずに
/// 次の要求へ渡せるという手順だけである。
const CURRENT_OBJECT_GUIDANCE: &str =
    "添えた対象の現在の姿は、読み直さずにそのまま次の要求の selector として使えます";

/// 一括適用で落ちた sub-operation の対象を返した失敗へ添える案内。
///
/// 同上。100 件を読み直さずに 1 件だけを差し替えられることを述べる。
const FAILED_OBJECT_GUIDANCE: &str =
    "添えた対象の現在の姿を使って、落ちた sub-operation の 1 件だけを差し替えて再要求できます";

/// 巻き戻しに失敗した一括適用へ添える案内。
///
/// **最も重大な失敗である。** 放置すると、次の編集が壊れた前提の上に積み上がる。
const CONSISTENCY_UNKNOWN_GUIDANCE: &str = "巻き戻しに失敗しており、プロジェクトが中途半端な状態の可能性があります。次の編集を行う前に必ず対象を読み直してください";

/// エラーの text content を組み立てる。
///
/// 行の並びは `code` → `correlation_id` → `details` の値 → 案内である。
///
/// **値を先に、案内を後に置く。** 値の行は `details` の key の数だけ並ぶ可変長で
/// あり、案内の行は「次に何をするか」を述べる手順である。手順を末尾へ固定すれば、
/// 読み手は行数を数えずに末尾を見れば次の一手が分かる。逆に案内を先へ置くと、
/// 可変長の値の行に押し流されて位置が定まらない。
///
/// 値の行と案内の行は別々の予算を持つ。値が上限で落ちても案内は落ちない。
pub fn text(error: &ErrorObject) -> String {
    let retry = if error.retryable {
        "リトライ可能"
    } else {
        "リトライ不可"
    };
    let correlation_id = error.correlation_id.as_deref().unwrap_or("-");
    let mut text = format!(
        "{} ({retry}): {}\ncorrelation_id={correlation_id}",
        error.code.as_snake_case(),
        clamp_chars(&error.message, MAX_MESSAGE_CHARS),
    );
    for line in detail_lines(&error.details)
        .into_iter()
        .chain(guidance(error))
    {
        text.push('\n');
        text.push_str(&line);
    }
    text
}

/// `details` を 1 行 1 key として描画する。
///
/// **key を名指しした分岐を持たない。** `details` へ値を足せば、この関数を
/// 変えずに要求元へ届く。key ごとに案内文を対応させる形は、値を足す側と text を
/// 組む側が別 crate にあるために片方だけが取り残される。
///
/// 描画するのは [`sanitize_details`] を通した後の値である。`details` の出所は
/// 接続先の応答と server 自身の組み立ての 2 つあり、前者しか選別を通っていない。
/// 描画の直前に通すことで、選別を経ない値が text へ出る経路を作らない。二度
/// 通しても結果は変わらない（切り詰め済みの値は再び切り詰められない）。
///
/// key の順は昇順に定まる。`serde_json` を `preserve_order` 無しで使っており
/// [`Map`] が `BTreeMap` であるため、走査がそのまま昇順になる。並び替えは持たない。
///
/// 値は compact JSON で書く。`structuredContent` に載る字面と一致させ、両方を
/// 読む要求元が同じものだと分かるようにするためである。
fn detail_lines(details: &Value) -> Vec<String> {
    let sanitized = sanitize_details(details, 0);
    let Some(map) = sanitized.as_object() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let mut used = 0;
    let mut dropped = 0;
    for (key, value) in map {
        let line = format!("details.{key}={value}");
        let chars = line.chars().count();
        // 予算を超えた行より後は、収まる大きさでも落とす。飛ばして拾うと
        // 「末尾から落とした」ではなくなり、どこまで届いたかが読めない。
        if dropped > 0 || used + chars > MAX_DETAIL_TEXT_CHARS {
            dropped += 1;
            continue;
        }
        used += chars;
        lines.push(line);
    }
    if dropped > 0 {
        // 黙って切ると、要求元は落とした key を「無い」と読む。
        lines.push(format!(
            "details のうち {dropped} 行を上限のため省略しました。全件は structuredContent を参照してください"
        ));
    }
    lines
}

/// 補助情報から、次に取るべき操作の案内を引く。
///
/// ここが答えるのは「次に何をするか」だけである。「何が起きたか」は
/// [`detail_lines`] が `details` から機械的に描画する。
fn guidance(error: &ErrorObject) -> Vec<String> {
    let details = &error.details;
    let mut lines = Vec::new();
    if details.get("reason").and_then(Value::as_str) == Some("layer_locked") {
        lines.push(LAYER_LOCKED_GUIDANCE.to_string());
    }
    if details.get("current_object").is_some() {
        lines.push(CURRENT_OBJECT_GUIDANCE.to_string());
    }
    lines.extend(batch_failure_lines(details));
    lines
}

/// 一括適用の失敗が、どこで落ちてどこまで戻せたかを示す行。
///
/// 一括適用は 1 回の要求で最大 100 件の変更を起こすため、失敗したという事実
/// だけでは要求元が何をすればよいか決められない。
fn batch_failure_lines(details: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(index) = details.get("failed_index").and_then(Value::as_u64) {
        lines.push(format!("operations[{index}] で失敗しました"));
    }
    match details.get("rolled_back").and_then(Value::as_bool) {
        Some(true) => lines.push("それまでに適用した変更は全て巻き戻しました".to_string()),
        Some(false) => lines.push(match details.get("rolled_back_count").and_then(Value::as_u64) {
            // 1 件の巻き戻し失敗は後続の巻き戻しを連鎖的に失敗させ得るため、
            // この値は実際に壊れている件数を過大に見積もり得る。
            Some(count) => format!(
                "巻き戻せたのは {count} 件です。この値は復旧の手掛かりであって、壊れている件数の計量ではありません"
            ),
            None => "巻き戻しに失敗した sub-operation があります".to_string(),
        }),
        None => {}
    }
    if details.get("failed_object").is_some() {
        lines.push(FAILED_OBJECT_GUIDANCE.to_string());
    }
    if details.get("consistency_unknown").and_then(Value::as_bool) == Some(true) {
        lines.push(CONSISTENCY_UNKNOWN_GUIDANCE.to_string());
    }
    lines
}

/// 接続先が返したエラーから、外部へ出してよい部分だけを残す。
///
/// `details` は key の断片で選別できるが、`message` は自由文のため長さを
/// 抑えるだけで内容は選別できない。message に何を書くかは接続先の責務である。
fn sanitize(error: ErrorObject) -> ErrorObject {
    let details = sanitize_details(&error.details, 0);
    let message = clamp_chars(&error.message, MAX_MESSAGE_CHARS);
    ErrorObject::new(error.code, message, error.retryable).with_details(details)
}

/// `details` から秘匿され得る値と過大な値を取り除く。
pub fn sanitize_details(value: &Value, depth: usize) -> Value {
    if depth >= MAX_DETAIL_DEPTH {
        return Value::Null;
    }
    match value {
        Value::Object(map) => {
            let mut sanitized = Map::new();
            for (key, item) in map {
                if is_sensitive_key(key) {
                    continue;
                }
                sanitized.insert(key.clone(), sanitize_details(item, depth + 1));
            }
            Value::Object(sanitized)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .take(MAX_DETAIL_ARRAY_ITEMS)
                .map(|item| sanitize_details(item, depth + 1))
                .collect(),
        ),
        Value::String(text) => Value::String(clamp_chars(text, MAX_DETAIL_STRING_CHARS)),
        other => other.clone(),
    }
}

/// key が秘匿対象の断片を含むか判定する。
fn is_sensitive_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    SENSITIVE_KEY_FRAGMENTS
        .iter()
        .any(|fragment| lowered.contains(fragment))
}

/// インスタンス解決の失敗を、内部構造を明かさない説明にする。
fn describe_resolve_error(error: &ResolveInstanceError) -> &'static str {
    match error {
        ResolveInstanceError::NotRegistered => {
            "指定された instance_id のインスタンスは登録されていません。list_instances で現在のインスタンスを取得してください"
        }
        ResolveInstanceError::Excluded(_) => {
            "指定されたインスタンスの生存確認に失敗しました。list_instances で一覧を取り直してください"
        }
        ResolveInstanceError::Rejected(_) => "インスタンスが要求を受け付けられませんでした",
    }
}

/// 要求送信の失敗を、内部構造を明かさない説明にする。
fn describe_pipe_error(error: &PipeClientError) -> &'static str {
    match error {
        PipeClientError::Timeout => "要求が期限内に完了しませんでした",
        PipeClientError::AuthenticationFailed => "インスタンスとの認証に失敗しました",
        PipeClientError::ProtocolMismatch => {
            "インスタンスのプロトコルバージョンが互換ではありません"
        }
        PipeClientError::Remote(_) => "インスタンスがエラーを返しました",
        _ => "インスタンスとの通信に失敗しました。list_instances で一覧を取り直してください",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::ExclusionReason;
    use aviutl2_mcp_core::{OPERATION_GET_OBJECT, OPERATION_MOVE_OBJECT};

    /// 接続先が名前を残すと決めている文字数。
    ///
    /// 定数から導かずに書く。導くと、切り詰めの長さを変えたときにこの表明も
    /// 一緒に動いてしまい、層の食い違いが戻っても落ちない。
    const INSTANCE_NAME_LIMIT: usize = 1_024;

    #[test]
    fn invalid_argument_is_not_retryable() {
        let error = invalid_argument("limit が範囲外です");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(!error.retryable);
    }

    #[test]
    fn not_registered_maps_to_instance_not_found() {
        let error = from_resolve_error(&ResolveInstanceError::NotRegistered);
        assert_eq!(error.code, ErrorCode::InstanceNotFound);
    }

    #[test]
    fn excluded_maps_to_instance_stale() {
        let error = from_resolve_error(&ResolveInstanceError::Excluded(
            ExclusionReason::PipeUnreachable,
        ));
        assert_eq!(error.code, ErrorCode::InstanceStale);
        assert!(error.retryable, "取り直しを促せるようリトライ可能にする");
    }

    #[test]
    fn rejected_keeps_retry_after_ms() {
        let remote = ErrorObject::new(ErrorCode::HostBusy, "起動処理中です", true)
            .with_details(serde_json::json!({ "retry_after_ms": 500 }));
        let error = from_resolve_error(&ResolveInstanceError::Rejected(Box::new(remote)));
        assert_eq!(error.code, ErrorCode::HostBusy);
        assert!(error.retryable);
        assert_eq!(error.details["retry_after_ms"], serde_json::json!(500));
    }

    #[test]
    fn remote_pipe_error_is_preserved() {
        let remote = ErrorObject::new(ErrorCode::PreconditionFailed, "scene が変化しました", true)
            .with_details(serde_json::json!({ "current_project_revision": 12 }));
        let error = from_pipe_error(
            &PipeClientError::Remote(Box::new(remote)),
            OPERATION_MOVE_OBJECT,
        );
        assert_eq!(error.code, ErrorCode::PreconditionFailed);
        assert_eq!(
            error.details["current_project_revision"],
            serde_json::json!(12)
        );
    }

    #[test]
    fn timeout_maps_to_timeout_code() {
        let error = from_pipe_error(&PipeClientError::Timeout, OPERATION_MOVE_OBJECT);
        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(error.retryable);
    }

    #[test]
    fn desynced_connection_maps_to_instance_stale() {
        let error = from_pipe_error(&PipeClientError::Desynced, OPERATION_MOVE_OBJECT);
        assert_eq!(error.code, ErrorCode::InstanceStale);
    }

    #[test]
    fn edit_failures_without_a_response_report_an_unknown_change() {
        // 応答を受け取れていない以上、要求が実行されたかは分からない。分からない
        // ことを名乗らないと、要求元は変更が無いものとして再送し、冪等でない
        // 作成や付与を重複させる。
        for pipe_error in [
            PipeClientError::Timeout,
            PipeClientError::Desynced,
            PipeClientError::ConnectFailed,
            PipeClientError::Framing,
            PipeClientError::InvalidResponse,
        ] {
            let error = from_pipe_error(&pipe_error, OPERATION_MOVE_OBJECT);
            assert_eq!(
                error.details["change_applied"],
                serde_json::json!("unknown"),
                "{pipe_error}"
            );
            assert_eq!(
                error.details["mutation_origin"],
                serde_json::json!("server"),
                "{pipe_error}"
            );
            assert_eq!(
                error.details["retry_requires"],
                serde_json::json!("refetch"),
                "{pipe_error}"
            );
        }
    }

    #[test]
    fn every_edit_operation_reports_an_unknown_change_on_a_timeout() {
        // operation を足したときに、変更の有無を添える経路から漏れないようにする。
        for operation in aviutl2_mcp_core::EditOperation::ALL {
            let error = from_pipe_error(&PipeClientError::Timeout, operation.as_str());
            assert_eq!(
                error.details["change_applied"],
                serde_json::json!("unknown"),
                "{}",
                operation.as_str()
            );
        }
    }

    #[test]
    fn read_failures_do_not_claim_anything_about_a_change() {
        // 読み取りは副作用を持たない。変更の有無という問いが成り立たないため、
        // 答えを名乗らない。
        let error = from_pipe_error(&PipeClientError::Timeout, OPERATION_GET_OBJECT);
        assert!(error.details.get("change_applied").is_none());
        assert!(error.details.get("mutation_origin").is_none());
        assert!(error.details.get("retry_requires").is_none());
    }

    #[test]
    fn a_response_from_the_instance_keeps_its_own_change_applied_hint() {
        // 接続先は実行前の期限超過だけを未適用と名乗れる。判別できた側の答えを
        // server の推測で塗り替えない。
        let remote = ErrorObject::new(ErrorCode::Timeout, "期限を超過しました", true).with_details(
            serde_json::json!({ "change_applied": "no", "mutation_origin": "plugin" }),
        );
        let error = from_pipe_error(
            &PipeClientError::Remote(Box::new(remote)),
            OPERATION_MOVE_OBJECT,
        );
        assert_eq!(error.details["change_applied"], serde_json::json!("no"));
        assert_eq!(
            error.details["mutation_origin"],
            serde_json::json!("plugin")
        );
    }

    #[test]
    fn sensitive_details_are_removed() {
        let remote = ErrorObject::new(ErrorCode::InternalError, "失敗", false).with_details(
            serde_json::json!({
                "auth_secret": "s3cret",
                "client_nonce": "abcd",
                "server_mac": "ffff",
                "pipe_name": r"\\.\pipe\aviutl2-mcp-1",
                "object_handle": 12345,
                "raw_pointer": "0x7ffdeadbeef",
                "project_path": r"C:\\Users\\me\\project.aup2",
                "alias": "[vo]",
                "retry_after_ms": 100,
            }),
        );
        let error = from_pipe_error(
            &PipeClientError::Remote(Box::new(remote)),
            OPERATION_MOVE_OBJECT,
        );
        let details = error.details.as_object().expect("details は object");
        for key in [
            "auth_secret",
            "client_nonce",
            "server_mac",
            "pipe_name",
            "object_handle",
            "raw_pointer",
            "project_path",
            "alias",
        ] {
            assert!(!details.contains_key(key), "{key} が残っています");
        }
        assert_eq!(details["retry_after_ms"], serde_json::json!(100));
    }

    #[test]
    fn sensitive_details_are_removed_from_nested_objects() {
        let details = sanitize_details(
            &serde_json::json!({
                "outer": { "auth_secret": "x", "revision": 3 },
                "list": [{ "server_nonce": "y", "count": 1 }],
            }),
            0,
        );
        assert_eq!(
            details,
            serde_json::json!({
                "outer": { "revision": 3 },
                "list": [{ "count": 1 }],
            })
        );
    }

    #[test]
    fn long_detail_strings_are_clamped() {
        let details = sanitize_details(
            &serde_json::json!({ "note": "あ".repeat(MAX_DETAIL_STRING_CHARS * 2) }),
            0,
        );
        let note = details["note"].as_str().expect("note は文字列");
        assert_eq!(note.chars().count(), MAX_DETAIL_STRING_CHARS);
    }

    #[test]
    fn a_name_kept_by_the_instance_reaches_the_caller_whole() {
        // 名前は要求を訂正するのに使う値である。接続先が残すと決めた長さを
        // server が黙って短くすると、長い名前を指定し直せなくなる。
        let name = "あ".repeat(INSTANCE_NAME_LIMIT);
        let remote = ErrorObject::new(ErrorCode::NotFound, "effect がありません", false)
            .with_details(serde_json::json!({ "effect_name": name }));
        let error = from_pipe_error(
            &PipeClientError::Remote(Box::new(remote)),
            OPERATION_MOVE_OBJECT,
        );
        assert_eq!(error.details["effect_name"], serde_json::json!(name));
    }

    #[test]
    fn a_current_object_reaches_the_caller_intact() {
        // 内容の食い違いは対象の現在の概要を返す。概要はセレクターを内包する
        // ため入れ子が深く、鍵の断片・深さ・文字数のどれかに掛かると、要求元は
        // そのまま送り返せる値を失う。
        let summary = aviutl2_mcp_core::ObjectSummary::new(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            aviutl2_mcp_core::ObjectFingerprintInput {
                scene_id: 0,
                layer: 2,
                frame_start: 120,
                frame_end: 240,
                name: Some("立ち絵"),
                alias: "[vo]",
            },
        );
        let remote = ErrorObject::new(ErrorCode::PreconditionFailed, "対象が変化しました", true)
            .with_details(serde_json::json!({
                "mismatch": "fingerprint",
                "current_object": summary,
            }));
        let error = from_pipe_error(
            &PipeClientError::Remote(Box::new(remote)),
            OPERATION_MOVE_OBJECT,
        );
        assert_eq!(
            error.details["current_object"],
            serde_json::to_value(&summary).expect("直列化できる")
        );
    }

    #[test]
    fn long_detail_arrays_are_truncated() {
        let items: Vec<Value> = (0..MAX_DETAIL_ARRAY_ITEMS * 3)
            .map(|i| serde_json::json!(i))
            .collect();
        let details = sanitize_details(&serde_json::json!({ "items": items }), 0);
        let truncated = details["items"].as_array().expect("items は配列");
        assert_eq!(truncated.len(), MAX_DETAIL_ARRAY_ITEMS);
        assert_eq!(truncated[0], serde_json::json!(0));
    }

    #[test]
    fn deep_details_are_dropped() {
        let mut value = serde_json::json!({ "leaf": 1 });
        for _ in 0..MAX_DETAIL_DEPTH + 2 {
            value = serde_json::json!({ "nested": value });
        }
        let sanitized = sanitize_details(&value, 0);
        let text = serde_json::to_string(&sanitized).expect("直列化できる");
        assert!(!text.contains("leaf"), "深すぎる値が残っています: {text}");
    }

    #[test]
    fn structured_error_carries_required_fields() {
        let error = with_correlation_id(
            invalid_argument("limit が範囲外です"),
            "0190abcd-1234-7def-89ab-0123456789ab",
        );
        let value = structured(&error);
        assert_eq!(value["code"], serde_json::json!("invalid_argument"));
        assert_eq!(value["retryable"], serde_json::json!(false));
        assert!(value["details"].is_object() || value["details"].is_null());
        assert_eq!(
            value["correlation_id"],
            serde_json::json!("0190abcd-1234-7def-89ab-0123456789ab")
        );
    }

    #[test]
    fn error_text_mentions_code_and_correlation_id() {
        let error = with_correlation_id(invalid_argument("範囲外"), "correlation");
        let text = text(&error);
        assert!(text.contains("invalid_argument"));
        assert!(text.contains("correlation"));
    }

    #[test]
    fn a_locked_layer_is_told_how_to_unlock_it() {
        // 再試行の分類は「この要求をどう扱うか」しか表せない。解決手段が別の
        // operation であることは text が伝える。
        let error = ErrorObject::new(ErrorCode::PreconditionFailed, "レイヤーがロック", true)
            .with_details(serde_json::json!({
                "reason": "layer_locked",
                "layer": 3,
                "retry_requires": "none",
            }));
        let text = text(&error);
        assert!(text.contains("set_layer_state"), "{text}");
        assert!(text.contains("ロックを解除"), "{text}");
    }

    #[test]
    fn a_content_mismatch_is_told_that_it_can_reuse_the_current_object() {
        // 現在の姿そのものが text に並び、かつ「読み直さずに次の要求へ渡せる」
        // という手順が添う。値だけでは次に何をするかが決まらず、手順だけでは
        // 材料が無い。両方が要る。
        let error = ErrorObject::new(ErrorCode::PreconditionFailed, "対象が変化しました", true)
            .with_details(serde_json::json!({
                "mismatch": "fingerprint",
                "current_object": { "layer": 2 },
                "retry_requires": "refetch",
            }));
        let text = text(&error);
        assert!(
            text.contains(r#"details.current_object={"layer":2}"#),
            "{text}"
        );
        assert!(
            text.contains("次の要求の selector として使えます"),
            "{text}"
        );
    }

    #[test]
    fn failures_without_a_next_step_are_not_given_one() {
        // 補助情報を持たない失敗には、値の行も案内の行も足さない。
        let text = text(&invalid_argument("limit が範囲外です"));
        assert!(!text.contains("details."), "{text}");
        assert!(!text.contains("set_layer_state"), "{text}");
        assert!(!text.contains("selector として使えます"), "{text}");
        assert!(!text.contains("operations["), "{text}");
        assert!(!text.contains("巻き戻し"), "{text}");
    }

    #[test]
    fn a_rolled_back_batch_says_where_it_stopped_and_that_nothing_is_left_behind() {
        let error = ErrorObject::new(ErrorCode::PreconditionFailed, "宛先が埋まっています", true)
            .with_details(serde_json::json!({
                "reason": "destination_occupied",
                "failed_index": 2,
                "rolled_back": true,
                "rolled_back_count": 2,
            }));
        let text = text(&error);
        assert!(text.contains("operations[2]"), "{text}");
        assert!(text.contains("全て巻き戻しました"), "{text}");
        // 巻き戻せた以上、読み直しは必須ではない。
        assert!(!text.contains("必ず対象を読み直して"), "{text}");
    }

    #[test]
    fn a_batch_that_could_not_be_rolled_back_demands_a_refetch() {
        // 最も重大な失敗であり、放置すると次の編集が壊れた前提の上に積み上がる。
        let error = ErrorObject::new(ErrorCode::SdkError, "巻き戻しに失敗しました", false)
            .with_details(serde_json::json!({
                "failed_index": 5,
                "rolled_back": false,
                "rolled_back_count": 3,
                "consistency_unknown": true,
            }));
        let text = text(&error);
        assert!(text.contains("operations[5]"), "{text}");
        assert!(text.contains("巻き戻せたのは 3 件"), "{text}");
        // 数値を信じて「3 件だけ直せばよい」と判断させない。
        assert!(text.contains("計量ではありません"), "{text}");
        assert!(text.contains("必ず対象を読み直して"), "{text}");
    }

    #[test]
    fn a_batch_object_mismatch_is_told_that_it_can_replace_a_single_operation() {
        // 100 件を読み直させないための値である。値そのものが text に並び、
        // かつ 1 件だけを差し替えられるという手順が添う。
        let error = ErrorObject::new(ErrorCode::PreconditionFailed, "対象が変化しました", true)
            .with_details(serde_json::json!({
                "mismatch": "fingerprint",
                "failed_index": 7,
                "failed_object": { "layer": 2 },
                "retry_requires": "refetch",
            }));
        let text = text(&error);
        assert!(
            text.contains(r#"details.failed_object={"layer":2}"#),
            "{text}"
        );
        assert!(text.contains("1 件だけを差し替えて"), "{text}");
    }

    #[test]
    fn a_batch_effect_mismatch_only_names_the_position() {
        // effect 側の不一致では差し替えの材料が付かない。付くものとして案内すると、
        // 要求元は存在しない値を探す。何が起きたかを述べる値の行は出るが、
        // 材料の在ることを前提にした案内は出ない。
        let error = ErrorObject::new(ErrorCode::PreconditionFailed, "対象が変化しました", true)
            .with_details(serde_json::json!({
                "mismatch": "effect_fingerprint",
                "failed_index": 7,
                "rolled_back": true,
            }));
        let text = text(&error);
        assert!(text.contains("operations[7]"), "{text}");
        assert!(
            text.contains(r#"details.mismatch="effect_fingerprint""#),
            "{text}"
        );
        assert!(!text.contains("details.failed_object="), "{text}");
        assert!(!text.contains("差し替えて"), "{text}");
    }

    #[test]
    fn a_detail_key_no_one_wrote_a_guidance_for_still_reaches_the_caller() {
        // 値の供給をホワイトリストへ戻さないための表明である。実在の key で
        // 書くと、たまたま案内が付いているだけでも通ってしまうため、案内を
        // 持ち得ない架空の key で確かめる。
        let error = ErrorObject::new(ErrorCode::SdkError, "失敗しました", false).with_details(
            serde_json::json!({
                "quokka_index": 7,
                "wombat_names": ["ひとつ", "ふたつ"],
            }),
        );
        let text = text(&error);
        assert!(text.contains("details.quokka_index=7"), "{text}");
        assert!(
            text.contains(r#"details.wombat_names=["ひとつ","ふたつ"]"#),
            "{text}"
        );
    }

    #[test]
    fn known_movements_and_observed_value_reach_the_caller() {
        // 補助情報へ載せたのに要求元へ届かなかった 2 件そのものを固定する。
        // 検査するのは key の名前ではなく値の字面である。
        let error = ErrorObject::new(
            ErrorCode::UnsupportedOperation,
            "移動方法の名前が既知の一覧にありません",
            false,
        )
        .with_details(serde_json::json!({
            "reason": "track_mode_unknown",
            "known_movements": [
                { "name": "直線移動", "writable": true },
                { "name": "移動無し", "writable": false },
            ],
            "observed_value": 4000,
        }));
        let text = text(&error);
        assert!(
            text.contains(
                r#"details.known_movements=[{"name":"直線移動","writable":true},{"name":"移動無し","writable":false}]"#
            ),
            "{text}"
        );
        assert!(text.contains("details.observed_value=4000"), "{text}");
        assert!(
            text.contains(r#"details.reason="track_mode_unknown""#),
            "{text}"
        );
    }

    #[test]
    fn detail_lines_are_reproducible_and_ordered_by_key() {
        // 同じ失敗が呼ぶたびに違う並びで返ると、要求元は差分を取れず、
        // 応答の比較でしか分からない退行を見逃す。
        let error = ErrorObject::new(ErrorCode::SdkError, "失敗しました", false)
            .with_details(serde_json::json!({ "zulu": 1, "alfa": 2, "mike": 3, "bravo": 4 }));
        let first = text(&error);
        assert_eq!(first, text(&error), "呼ぶたびに並びが変わっています");

        let keys: Vec<&str> = first
            .lines()
            .filter_map(|line| line.strip_prefix("details."))
            .filter_map(|line| line.split_once('='))
            .map(|(key, _)| key)
            .collect();
        assert_eq!(keys, ["alfa", "bravo", "mike", "zulu"], "{first}");
    }

    #[test]
    fn sensitive_details_are_absent_from_the_text_content() {
        // 選別は `structuredContent` の側だけの約束ではない。選別を経ない
        // 補助情報が text へ出る経路を作らない。
        let error = ErrorObject::new(ErrorCode::InternalError, "失敗しました", false).with_details(
            serde_json::json!({
                "auth_secret": "s3cret",
                "pipe_name": r"\\.\pipe\aviutl2-mcp-1",
                "project_path": r"C:\Users\me\project.aup2",
                "retry_after_ms": 100,
            }),
        );
        let text = text(&error);
        for absent in [
            "auth_secret",
            "s3cret",
            "pipe_name",
            "aviutl2-mcp-1",
            "project_path",
            "project.aup2",
        ] {
            assert!(!text.contains(absent), "{absent} が残っています: {text}");
        }
        assert!(text.contains("details.retry_after_ms=100"), "{text}");
    }

    #[test]
    fn details_beyond_the_budget_are_dropped_whole_and_the_loss_is_stated() {
        // 途中で切った JSON は要求元が読めず、黙って切ると落とした key を
        // 「無い」と読む。落とすのは行の単位であり、落としたことを述べる。
        const KEYS: usize = 40;
        let mut details = Map::new();
        for index in 0..KEYS {
            details.insert(
                format!("filler_{index:02}"),
                Value::String("あ".repeat(MAX_DETAIL_STRING_CHARS)),
            );
        }
        let error = ErrorObject::new(ErrorCode::SdkError, "失敗しました", false)
            .with_details(Value::Object(details));
        let text = text(&error);

        let kept: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with("details."))
            .collect();
        assert!(!kept.is_empty(), "1 行も残っていません");
        assert!(kept.len() < KEYS, "上限が効いていません");

        let budget: usize = kept.iter().map(|line| line.chars().count()).sum();
        assert!(budget <= MAX_DETAIL_TEXT_CHARS, "{budget} 文字残っています");

        for line in &kept {
            let (_, encoded) = line
                .strip_prefix("details.")
                .and_then(|line| line.split_once('='))
                .expect("値の行は key=value の形である");
            serde_json::from_str::<Value>(encoded)
                .unwrap_or_else(|error| panic!("JSON が途中で切れています: {line} ({error})"));
        }

        let notice = text
            .lines()
            .find(|line| line.starts_with("details のうち"))
            .expect("省略を述べる行がありません");
        assert!(
            notice.contains(&format!("{} 行", KEYS - kept.len())),
            "{notice}"
        );
    }

    #[test]
    fn a_locked_layer_gets_both_the_values_and_the_next_step() {
        // 値の行は「何が起きたか」を、案内の行は「次に何をするか」を答える。
        // 前者を足したことで後者が消えると、要求元は別 operation を挟めば
        // 解けることを知る手段を失う。
        let error = ErrorObject::new(ErrorCode::PreconditionFailed, "レイヤーがロック", true)
            .with_details(serde_json::json!({
                "reason": "layer_locked",
                "layer": 3,
                "retry_requires": "none",
            }));
        let text = text(&error);
        assert!(text.contains("details.layer=3"), "{text}");
        assert!(text.contains(r#"details.reason="layer_locked""#), "{text}");
        assert!(text.contains(LAYER_LOCKED_GUIDANCE), "{text}");
    }

    #[test]
    fn the_next_step_is_placed_after_every_value_line_and_outlives_their_budget() {
        // 案内は「次に何をするか」を述べる手順であり、値の行は key の数だけ並ぶ
        // 可変長である。手順を末尾へ固定しないと、読み手は行数を数えないと
        // 次の一手を見つけられない。
        let error = ErrorObject::new(ErrorCode::PreconditionFailed, "レイヤーがロック", true)
            .with_details(serde_json::json!({
                "reason": "layer_locked",
                "layer": 3,
                "zulu": "昇順で最後に並ぶ値",
            }));
        let ordered = text(&error);
        let last_value = ordered.rfind("\ndetails.").expect("値の行が出ていません");
        let next_step = ordered
            .find(LAYER_LOCKED_GUIDANCE)
            .expect("案内の行が出ていません");
        assert!(
            next_step > last_value,
            "案内が値より前にあります: {ordered}"
        );

        // 値の行と案内の行は別々の予算を持つ。値が上限で落ちても手順は落ちない
        // ——手順を失うと、要求元には失敗したという事実しか残らない。
        //
        // 値の行で予算をちょうど使い切る。余りを残すと、案内を値と同じ予算から
        // 支払う実装でもその余りに収まってしまい、別々であることを見られない。
        const LINE_CHARS: usize = 50;
        const VALUE_CHARS: usize = LINE_CHARS - "details.filler_00=\"\"".len();
        let kept = MAX_DETAIL_TEXT_CHARS / LINE_CHARS;
        assert_eq!(
            MAX_DETAIL_TEXT_CHARS % LINE_CHARS,
            0,
            "予算を割り切る行長でないと余りが残る"
        );

        let mut details = Map::new();
        for index in 0..kept + 20 {
            details.insert(
                format!("filler_{index:02}"),
                Value::String("x".repeat(VALUE_CHARS)),
            );
        }
        details.insert("reason".to_string(), serde_json::json!("layer_locked"));
        let crowded = text(
            &ErrorObject::new(ErrorCode::PreconditionFailed, "レイヤーがロック", true)
                .with_details(Value::Object(details)),
        );
        assert!(
            crowded.contains("details のうち"),
            "省略が起きていません: {crowded}"
        );
        let used: usize = crowded
            .lines()
            .filter(|line| line.starts_with("details."))
            .map(|line| line.chars().count())
            .sum();
        assert_eq!(used, MAX_DETAIL_TEXT_CHARS, "予算に余りが残っています");
        // 引き金になった値の行そのものが落ちてもなお案内は残る。
        assert!(
            !crowded.contains(r#"details.reason="layer_locked""#),
            "{crowded}"
        );
        assert!(crowded.contains(LAYER_LOCKED_GUIDANCE), "{crowded}");
    }

    #[test]
    fn details_that_are_not_an_object_produce_no_lines() {
        // 補助情報は自由な形の値であり、object とは限らない。key を持たない
        // 値へ `details.` の行を作らない。
        let error = ErrorObject::new(ErrorCode::SdkError, "失敗しました", false)
            .with_details(serde_json::json!("ただの文字列"));
        let text = text(&error);
        assert!(!text.contains("details."), "{text}");
    }

    #[test]
    fn sanitizing_details_twice_changes_nothing() {
        // 描画の直前にも選別を通すため、選別済みの値を再び通す経路が常に走る。
        // 二度目が値を削るなら、text と `structuredContent` の字面がずれる。
        let mut deep = serde_json::json!({ "leaf": 1 });
        for _ in 0..MAX_DETAIL_DEPTH + 2 {
            deep = serde_json::json!({ "nested": deep });
        }
        let details = serde_json::json!({
            "auth_secret": "s3cret",
            "note": "あ".repeat(MAX_DETAIL_STRING_CHARS * 2),
            "items": (0..MAX_DETAIL_ARRAY_ITEMS * 3).collect::<Vec<_>>(),
            "deep": deep,
        });
        let once = sanitize_details(&details, 0);
        assert_eq!(sanitize_details(&once, 0), once);
    }

    #[test]
    fn long_remote_message_is_clamped() {
        let remote = ErrorObject::new(ErrorCode::SdkError, "え".repeat(10_000), false);
        let error = from_pipe_error(
            &PipeClientError::Remote(Box::new(remote)),
            OPERATION_MOVE_OBJECT,
        );
        assert!(error.message.chars().count() <= MAX_MESSAGE_CHARS);
    }
}
