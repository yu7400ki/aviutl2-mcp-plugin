//! 接続ごとの handshake と要求処理。
//!
//! 全メッセージは frame 形式でやり取りする。handshake 成功後にのみ
//! `RequestEnvelope` を受理する。panic は `catch_unwind` で捕捉し、
//! 当該接続のみ切断する。
//!
//! 読み取り要求は受け取ったその場で実行する。1 接続は接続受理スレッド上で
//! 同期的に処理され、処理中は次の接続を受理しないため、読み取りは構造として
//! 直列化される。同時実行数を数える仕組みも実行待ちのキューも持たないので、
//! 受付から実行までの間に要求を取り消す余地は無く、飽和による滞留も生じない。

use crate::lifecycle::Lifecycle;
use crate::pipe::PipeStream;
use crate::read::{ReadAdapter, ReadError};
use anyhow::{Context, Result};
use aviutl2_mcp_core::{
    ClientAuth, ClientHello, ErrorCode, ErrorObject, GetCurrentSceneParams, GetCurrentSceneResult,
    GetEditInfoParams, GetObjectParams, InstanceId, InstanceState, ListAvailableEffectsParams,
    ListAvailableEffectsResult, ListLayersParams, ListLayersResult, ListObjectsParams,
    ListObjectsResult, Nonce, OPERATION_GET_CURRENT_SCENE, OPERATION_GET_EDIT_INFO,
    OPERATION_GET_OBJECT, OPERATION_LIST_AVAILABLE_EFFECTS, OPERATION_LIST_LAYERS,
    OPERATION_LIST_OBJECTS, ObjectFilterError, PageError, PageRequest, ProtocolVersion,
    RequestEnvelope, RequestId, ResponseEnvelope, ResponseKind, ResponseResult, compute_client_mac,
    compute_server_mac, deserialize_json, negotiate, take_page, verify_mac,
};
use chrono::Utc;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// handshake（M1 受信 〜 M3 検証）全体に許す上限。
///
/// handshake は接続確立直後に 3 往復で完結する軽量な処理であり、
/// クライアントの待ち時間は含まない。未応答のクライアントが待受を占有する
/// 時間をこの値に抑える。
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// 認証済み接続で次の要求フレームを待つ上限。
///
/// 1 接続は handshake → 要求 → 応答の直列で完結し、クライアントは応答受信後に
/// 切断する。したがってこの待機は実質「相手の切断（EOF）を受け取るまで」であり、
/// 通常はミリ秒で終わる。待受インスタンスは 1 本だけで、1 接続の処理中は
/// 新たな接続を受理できないため、黙り込んだクライアントが占有できる時間を
/// この値に抑える。
const REQUEST_IDLE_TIMEOUT: Duration = Duration::from_secs(15);

/// 1 フレームの送信に許す上限。
///
/// 受信側がバッファを読み出さない場合でも送信側が滞留しないようにする。
/// 要求が deadline を指定した場合は、この上限と deadline の短い方を採用する。
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// 読み取り operation の実行に許す上限。
///
/// 要求が deadline を指定した場合は、この上限と deadline の短い方を採用する。
/// 応答の送信はこの期限とは別に区切る。読み取りに費やした時間を差し引いた残りと
/// [`WRITE_TIMEOUT`] の短い方を送信へ充てることで、読み取りが長引いた分だけ
/// 送信の余地が奪われる。
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// 読み取りを受け付けられない状態で案内する再試行間隔（ミリ秒）。
///
/// 起動処理も終了処理も利用者の操作を待たずに進むため、待ち時間は短く採る。
const HOST_BUSY_RETRY_AFTER_MS: u64 = 500;

/// 1 接続の処理を panic boundary で包んで実行する。
///
/// 読み取り口は全接続で共有し、SDK 呼び出しとプロジェクト状態の参照は
/// その内側へ閉じる。
pub fn handle_connection(
    stream: PipeStream,
    lifecycle: Arc<Lifecycle>,
    read_adapter: Arc<dyn ReadAdapter>,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Err(e) = run_connection(&stream, &lifecycle, read_adapter.as_ref()) {
            tracing::warn!("接続処理を終了しました: {e:?}");
        }
    }));
    if result.is_err() {
        tracing::error!("接続処理中に panic が発生しました");
    }
}

/// 接続単位のメインループ。
fn run_connection(
    stream: &PipeStream,
    lifecycle: &Lifecycle,
    read_adapter: &dyn ReadAdapter,
) -> Result<()> {
    let negotiated_version = perform_handshake(stream, lifecycle)?;
    run_request_loop(stream, lifecycle, read_adapter, negotiated_version)
}

/// handshake を実行し、採用プロトコルバージョンを返す。
///
/// 検証に失敗した場合はエラー応答を返さずに `Err` を返し、呼び出し元が接続を
/// 切断する。未認証の相手へ失敗理由を開示しないため、理由はローカルログにのみ
/// 記録する。`auth_secret`・nonce・MAC はログに出さない。
fn perform_handshake(stream: &PipeStream, lifecycle: &Lifecycle) -> Result<ProtocolVersion> {
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;

    let client_hello = read_frame_as::<ClientHello>(stream, deadline)
        .context("ClientHello の受信に失敗しました")?;

    if client_hello.instance_id != lifecycle.instance_id() {
        anyhow::bail!("ClientHello の instance_id が一致しないため接続を切断します");
    }

    let negotiated = negotiate(ProtocolVersion::CURRENT, client_hello.protocol_version)
        .map_err(|_| anyhow::anyhow!("プロトコルバージョンが一致しないため接続を切断します"))?;

    let server_nonce = Nonce::generate();
    let server_mac = compute_server_mac(
        lifecycle.auth_secret().as_bytes(),
        &client_hello.client_nonce,
        &server_nonce,
        &lifecycle.instance_id(),
        &negotiated,
    );

    let server_auth = aviutl2_mcp_core::ServerAuth {
        protocol_version: negotiated,
        instance_id: lifecycle.instance_id(),
        server_nonce,
        pid: lifecycle.descriptor().pid,
        process_created_at: lifecycle.descriptor().process_created_at.clone(),
        server_mac,
    };

    let server_auth_body =
        serde_json::to_vec(&server_auth).context("ServerAuth の JSON 直列化に失敗しました")?;
    stream
        .write_frame(&server_auth_body, deadline)
        .context("ServerAuth の送信に失敗しました")?;

    let client_auth =
        read_frame_as::<ClientAuth>(stream, deadline).context("ClientAuth の受信に失敗しました")?;
    let expected_client_mac = compute_client_mac(
        lifecycle.auth_secret().as_bytes(),
        &server_auth.server_nonce,
        &client_hello.client_nonce,
    );

    if !verify_mac(&expected_client_mac, &client_auth.client_mac) {
        anyhow::bail!("ClientAuth の MAC 検証に失敗したため接続を切断します");
    }

    Ok(negotiated)
}

/// 認証済み接続での要求処理ループ。
///
/// 応答送信直後には閉じず、次の受信でクライアント切断（EOF）か期限超過を
/// 待ってから抜ける。送信済み応答がクライアントに読まれる前にハンドルを
/// 破棄しないための構造。
fn run_request_loop(
    stream: &PipeStream,
    lifecycle: &Lifecycle,
    read_adapter: &dyn ReadAdapter,
    negotiated_version: ProtocolVersion,
) -> Result<()> {
    loop {
        if lifecycle.state() == InstanceState::Draining {
            // draining では新規要求を受け付けず、接続を閉じる。
            break;
        }

        let deadline = Instant::now() + REQUEST_IDLE_TIMEOUT;
        let body = match stream
            .read_frame(deadline)
            .context("要求フレームの受信に失敗しました")?
        {
            Some(b) => b,
            None => break,
        };

        let request: RequestEnvelope = deserialize_json(&body)
            .map_err(|e| anyhow::anyhow!("RequestEnvelope のデコードに失敗しました: {e}"))?;

        match classify_version(negotiated_version, request.protocol_version) {
            VersionCheck::Compatible => {}
            VersionCheck::MinorTooHigh => {
                send_error(
                    stream,
                    negotiated_version,
                    request.request_id,
                    request.instance_id,
                    error_object(
                        ErrorCode::ProtocolMismatch,
                        "要求の MINOR が交渉結果を超えています",
                    ),
                )?;
                continue;
            }
            VersionCheck::MajorMismatch => {
                // MAJOR 不一致は互換性が無く接続を継続できない。handshake は
                // 完了しているため理由を 1 度返し、以降の要求は処理せずに
                // クライアントの切断を待ってから閉じる。
                send_error(
                    stream,
                    negotiated_version,
                    request.request_id,
                    request.instance_id,
                    error_object(
                        ErrorCode::ProtocolMismatch,
                        "要求の MAJOR が交渉結果と一致しません",
                    ),
                )?;
                await_peer_close(stream);
                break;
            }
        }

        if request.instance_id != lifecycle.instance_id() {
            send_error(
                stream,
                negotiated_version,
                request.request_id,
                request.instance_id,
                error_object(
                    ErrorCode::InstanceNotFound,
                    "インスタンス ID が一致しません",
                ),
            )?;
            continue;
        }

        // operation の対応付けはライフサイクル状態の確認より先に行う。未対応の
        // operation は状態が変わっても対応されることが無く、状態由来の再試行可能な
        // エラーで返すと要求元に解消しない再試行を促してしまう。
        let operation = match classify_operation(&request.operation) {
            Ok(operation) => operation,
            Err(error) => {
                send_error(
                    stream,
                    negotiated_version,
                    request.request_id,
                    request.instance_id,
                    error,
                )?;
                continue;
            }
        };

        // 期限は operation の実行に対する制約であり、要求自体の妥当性検証
        // （version・instance・operation）を通した後に評価する。妥当性の誤りは
        // 再試行しても解消しないため、再試行可能な `timeout` より先に返す。
        match operation {
            Operation::Ping => {
                // 生存確認は状態を読むだけで、実行に費やす時間を持たない。
                // 期限は応答の送信にそのまま充てる。
                let deadline = match resolve_request_deadline(
                    Instant::now(),
                    Utc::now().timestamp_millis(),
                    WRITE_TIMEOUT,
                    request.deadline_unix_ms,
                ) {
                    RequestDeadline::Within(deadline) => deadline,
                    RequestDeadline::Exceeded => {
                        send_error(
                            stream,
                            negotiated_version,
                            request.request_id,
                            request.instance_id,
                            timeout_before_execution(),
                        )?;
                        continue;
                    }
                };
                let response = ResponseEnvelope::pong(
                    negotiated_version,
                    request.request_id,
                    lifecycle.instance_id(),
                    lifecycle.state(),
                );
                send_response(stream, &response, deadline)?;
            }
            Operation::Read(operation) => {
                let read_deadline = resolve_request_deadline(
                    Instant::now(),
                    Utc::now().timestamp_millis(),
                    READ_TIMEOUT,
                    request.deadline_unix_ms,
                );
                let outcome = execute_read(
                    read_adapter,
                    &lifecycle.state(),
                    operation,
                    &request.params,
                    read_deadline,
                );

                let read_response = resolve_read_response(
                    Instant::now(),
                    Utc::now().timestamp_millis(),
                    read_deadline,
                    request.deadline_unix_ms,
                    outcome,
                );

                if read_response.discarded {
                    tracing::warn!(
                        request_id = ?request.request_id,
                        operation = %request.operation,
                        "deadline を超過したため読み取り結果を破棄しました"
                    );
                } else if let Err(error) = &read_response.outcome {
                    // 失敗を返した事実だけをローカルへ残す。要求元の報告から
                    // plugin 側の状況を辿る手掛かりになる。
                    tracing::debug!(
                        request_id = ?request.request_id,
                        operation = %request.operation,
                        code = %error.code,
                        "読み取り要求を失敗として返します"
                    );
                }

                let response = response_envelope(
                    negotiated_version,
                    request.request_id,
                    lifecycle.instance_id(),
                    read_response.outcome,
                );
                send_response(stream, &response, read_response.deadline)?;
            }
        }
    }

    Ok(())
}

/// 受理できる operation。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    /// 生存確認。ライフサイクル状態を問わず受け付ける。
    Ping,
    /// 読み取り。受け付けられるライフサイクル状態でのみ実行する。
    Read(ReadOperation),
}

/// 読み取り operation。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadOperation {
    GetEditInfo,
    GetCurrentScene,
    ListLayers,
    ListObjects,
    GetObject,
    ListAvailableEffects,
}

/// operation 名を処理経路へ対応付ける。
fn classify_operation(name: &str) -> Result<Operation, ErrorObject> {
    let operation = match name {
        "ping" => return Ok(Operation::Ping),
        OPERATION_GET_EDIT_INFO => ReadOperation::GetEditInfo,
        OPERATION_GET_CURRENT_SCENE => ReadOperation::GetCurrentScene,
        OPERATION_LIST_LAYERS => ReadOperation::ListLayers,
        OPERATION_LIST_OBJECTS => ReadOperation::ListObjects,
        OPERATION_GET_OBJECT => ReadOperation::GetObject,
        OPERATION_LIST_AVAILABLE_EFFECTS => ReadOperation::ListAvailableEffects,
        _ => {
            return Err(error_object(
                ErrorCode::UnsupportedOperation,
                "未対応の operation です",
            ));
        }
    };
    Ok(Operation::Read(operation))
}

/// 受付判定と期限判定を通してから読み取りを実行する。
///
/// 実行前に期限を超過している要求は読み取り口へ渡さない。読み取りは開始すると
/// 参照区間の内側まで進み、途中で打ち切れないためである。
fn execute_read(
    adapter: &dyn ReadAdapter,
    state: &InstanceState,
    operation: ReadOperation,
    params: &Value,
    deadline: RequestDeadline,
) -> Result<Value, ErrorObject> {
    admit_read(state)?;
    if deadline == RequestDeadline::Exceeded {
        // 未開始の要求は中止する。副作用が無いため再試行可能として返す。
        return Err(timeout_before_execution());
    }
    dispatch_read(adapter, operation, params)
}

/// 実行前に期限を超過していた要求へ返すエラー。
fn timeout_before_execution() -> ErrorObject {
    error_object(
        ErrorCode::Timeout,
        "要求の deadline を超過したため処理しません",
    )
}

/// 実行後に期限を超過し、結果を捨てた要求へ返すエラー。
fn timeout_after_execution() -> ErrorObject {
    error_object(
        ErrorCode::Timeout,
        "要求の deadline を超過したため読み取り結果を破棄しました",
    )
}

/// 読み取りの結果を応答として送るかどうか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendDecision {
    /// この期限までに応答を送る。
    Send(Instant),
    /// 期限を超過したため結果を捨てる。
    Discard,
}

/// 読み取りを終えた時点で、結果を送るか捨てるかを決める。
///
/// 読み取りの期限を過ぎていれば結果を捨てる。読み取りは編集を伴わないため、
/// 捨てても中途半端な状態は残らない。期限内であれば、要求の残り時間と送信上限の
/// 短い方を応答送信の期限とする。
///
/// 実行前に期限を超過していた要求は読み取りを行っておらず、捨てる結果を持たない。
/// 超過した理由を返せるよう、送信上限だけで送る。
fn decide_send(
    now: Instant,
    now_unix_ms: i64,
    read_deadline: RequestDeadline,
    deadline_unix_ms: Option<u64>,
) -> SendDecision {
    let RequestDeadline::Within(read_deadline) = read_deadline else {
        return SendDecision::Send(now + WRITE_TIMEOUT);
    };
    if now >= read_deadline {
        return SendDecision::Discard;
    }
    match resolve_request_deadline(now, now_unix_ms, WRITE_TIMEOUT, deadline_unix_ms) {
        RequestDeadline::Within(deadline) => SendDecision::Send(deadline),
        RequestDeadline::Exceeded => SendDecision::Discard,
    }
}

/// 読み取りを終えた時点で決まる応答の内容と送信期限。
struct ReadResponse {
    /// 応答へ載せる内容。
    outcome: Result<Value, ErrorObject>,
    /// 応答送信の期限。
    deadline: Instant,
    /// 期限超過により読み取り結果を捨てたか。監査ログを残す条件に使う。
    discarded: bool,
}

/// 読み取りの結果を、期限の判定と突き合わせて応答の形にまとめる。
///
/// 期限内であれば結果をそのまま送る。期限を超過していた場合に捨てるのは、
/// 成功した読み取り結果だけである。読み取りが失敗していた場合は捨てる結果が
/// 無く、期限超過へ置き換えると失敗の理由も再試行の可否も書き換わってしまう
/// ため、失敗をそのまま返す。
///
/// 期限を超過した後の送信は、要求の期限ではなくサーバー側の送信上限で区切る。
/// 超過を伝える応答まで当の期限で打ち切ると、要求元は理由を得られない。
fn resolve_read_response(
    now: Instant,
    now_unix_ms: i64,
    read_deadline: RequestDeadline,
    deadline_unix_ms: Option<u64>,
    outcome: Result<Value, ErrorObject>,
) -> ReadResponse {
    match decide_send(now, now_unix_ms, read_deadline, deadline_unix_ms) {
        SendDecision::Send(deadline) => ReadResponse {
            outcome,
            deadline,
            discarded: false,
        },
        SendDecision::Discard => match outcome {
            Ok(_) => ReadResponse {
                outcome: Err(timeout_after_execution()),
                deadline: now + WRITE_TIMEOUT,
                discarded: true,
            },
            Err(error) => ReadResponse {
                outcome: Err(error),
                deadline: now + WRITE_TIMEOUT,
                discarded: false,
            },
        },
    }
}

/// ライフサイクル状態が読み取りを受け付けられるかを判定する。
///
/// 起動処理中は編集ハンドルが読み取り API を受け付けられず、呼ぶこと自体が
/// 許されない。終了処理中は新規要求を受け付けない。いずれも要求内容の誤りでは
/// ないため、間隔を空けた再試行を案内する。再生・出力中の判別は編集状態を
/// 確かめられる読み取り口の責務であり、`busy` はここでは通す。
fn admit_read(state: &InstanceState) -> Result<(), ErrorObject> {
    match state {
        InstanceState::Ready | InstanceState::Busy => Ok(()),
        InstanceState::Starting => Err(host_busy("起動処理中のため読み取りを受け付けられません")),
        InstanceState::Draining => Err(host_busy("終了処理中のため読み取りを受け付けられません")),
        // 接続済みの経路では観測されない。この状態へ移る際は descriptor の削除と
        // pipe の切断が先立ち、要求元は接続の失敗として先に検出する。同じ
        // インスタンスが戻ることはないため再試行の間隔を案内せず、インスタンスを
        // 探し直すべき状態として返す。
        InstanceState::Gone => Err(error_object(
            ErrorCode::InstanceStale,
            "インスタンスは既に終了しています",
        )),
        InstanceState::Unknown(_) => Err(host_busy("読み取りを受け付けられない状態です")),
    }
}

/// 一時的に読み取りを受け付けられないことを、再試行の案内つきで返す。
fn host_busy(message: &str) -> ErrorObject {
    error_object(ErrorCode::HostBusy, message)
        .with_details(json!({ "retry_after_ms": HOST_BUSY_RETRY_AFTER_MS }))
}

/// 読み取りを実行し、応答へ載せる result を組み立てる。
///
/// params の復号とページ指定の検証は読み取り口を呼ぶ前に済ませる。要求の誤りで
/// SDK へ触れないようにするためである。
///
/// 読み取り口は SDK の参照区間を抜けてから所有型の DTO を返す。ページの切り出しと
/// JSON への変換はいずれもその外側で行い、参照区間の内側には持ち込まない。
fn dispatch_read(
    adapter: &dyn ReadAdapter,
    operation: ReadOperation,
    params: &Value,
) -> Result<Value, ErrorObject> {
    match operation {
        ReadOperation::GetEditInfo => {
            decode_params::<GetEditInfoParams>(params)?;
            to_result(&adapter.get_edit_info().map_err(read_error)?)
        }
        ReadOperation::GetCurrentScene => {
            decode_params::<GetCurrentSceneParams>(params)?;
            let (scene, project_revision) = adapter.get_current_scene().map_err(read_error)?;
            to_result(&GetCurrentSceneResult {
                scene,
                project_revision,
            })
        }
        ReadOperation::ListLayers => {
            let params: ListLayersParams = decode_params(params)?;
            params.page.validate().map_err(page_error)?;
            let snapshot = adapter
                .list_layers(params.expected_scene_id)
                .map_err(read_error)?;
            let (items, page) =
                take_page(&snapshot.items, &params.page, snapshot.snapshot_revision)
                    .map_err(page_error)?;
            to_result(&ListLayersResult { items, page })
        }
        ReadOperation::ListObjects => {
            let params: ListObjectsParams = decode_params(params)?;
            params.page.validate().map_err(page_error)?;
            if let Some(filter) = &params.filter {
                filter.validate().map_err(filter_error)?;
            }
            let snapshot = adapter
                .list_objects(params.expected_scene_id, params.filter.as_ref())
                .map_err(read_error)?;
            let (items, page) =
                take_page(&snapshot.items, &params.page, snapshot.snapshot_revision)
                    .map_err(page_error)?;
            to_result(&ListObjectsResult { items, page })
        }
        ReadOperation::GetObject => {
            let params: GetObjectParams = decode_params(params)?;
            to_result(&adapter.get_object(&params.selector).map_err(read_error)?)
        }
        ReadOperation::ListAvailableEffects => {
            let params: ListAvailableEffectsParams = decode_params(params)?;
            params.page.validate().map_err(page_error)?;
            let snapshot = adapter
                .list_available_effects(params.effect_type.as_ref())
                .map_err(read_error)?;
            let (items, page) = take_page(
                &snapshot.items,
                &catalog_page_request(&params.page),
                snapshot.snapshot_revision,
            )
            .map_err(page_error)?;
            to_result(&ListAvailableEffectsResult { items, page })
        }
    }
}

/// 登録済み effect の一覧に対するページ要求から revision の照合指定を落とす。
///
/// この一覧は登録済みプラグインの集合であり、プロジェクトの編集内容から独立して
/// いる。要求元が前ページの revision を送り返しても照合しない。照合すると、一覧と
/// 無関係な編集で値が進んだだけでページ間の照合が食い違い、要求元は先頭からの
/// 取り直しを強いられる。一方でカタログ自身の変化はその値に現れないため、照合
/// しても取りこぼしは防げない。
///
/// 応答へ載せる revision は落とさない。それは列挙を始めた時点のプロジェクト
/// revision であり、ページのメタ情報が表す意味そのものである。照合に使えない
/// ことを表す固定値へ置き換えても、実在し得る revision と区別が付かない。
fn catalog_page_request(page: &PageRequest) -> PageRequest {
    PageRequest {
        snapshot_revision: None,
        ..*page
    }
}

/// operation 別の params へ復号する。
///
/// 失敗の説明には、不足したフィールド名や受理できないフィールド名が含まれる。
/// いずれも要求元が送った内容と入力型の定義だけに由来し、秘匿値は含まない。
fn decode_params<T: DeserializeOwned>(params: &Value) -> Result<T, ErrorObject> {
    serde_json::from_value(params.clone()).map_err(|e| {
        error_object(
            ErrorCode::InvalidArgument,
            format!("params の解釈に失敗しました: {e}"),
        )
    })
}

/// 読み取り結果を応答へ載せる JSON へ変換する。
///
/// 変換できるかは DTO の定義だけで決まり、要求元には手立てが無い。失敗の詳細は
/// ローカルのログにのみ残す。
fn to_result<T: Serialize>(value: &T) -> Result<Value, ErrorObject> {
    serde_json::to_value(value).map_err(|e| {
        tracing::error!("読み取り結果の JSON 変換に失敗しました: {e}");
        error_object(
            ErrorCode::InternalError,
            "読み取り結果を応答へ変換できませんでした",
        )
    })
}

/// 読み取りの失敗を応答用のエラーへ変換する。
///
/// 再試行間隔は読み取りの補助情報に含まれるため、ここで重ねて載せない。
fn read_error(error: ReadError) -> ErrorObject {
    ErrorObject::new(error.error_code(), error.to_string(), error.retryable())
        .with_details(error.details())
}

/// ページ指定の失敗を応答用のエラーへ変換する。
fn page_error(error: PageError) -> ErrorObject {
    match error {
        PageError::LimitOutOfRange(_) => {
            error_object(ErrorCode::InvalidArgument, error.to_string())
        }
        PageError::SnapshotRevisionMismatch { requested, current } => error_object(
            ErrorCode::PreconditionFailed,
            "一覧が変化したため、先頭のページから取り直してください",
        )
        .with_details(json!({
            "requested_snapshot_revision": requested,
            "current_snapshot_revision": current,
        })),
    }
}

/// 絞り込み条件の失敗を応答用のエラーへ変換する。
fn filter_error(error: ObjectFilterError) -> ErrorObject {
    error_object(ErrorCode::InvalidArgument, error.to_string())
}

/// 送信済み応答が読み取られるのを待ってから接続を閉じるための待機。
///
/// クライアント切断（EOF）か期限超過まで受信を続け、受け取ったフレームは
/// 処理せずに捨てる。応答送信の直後にハンドルを破棄すると、
/// `DisconnectNamedPipe` が pipe バッファの未読データを捨てるため、
/// クライアントは応答ではなく切断を観測してしまう。
///
/// 期限超過や I/O エラーはいずれも接続を閉じる契機であり、呼び出し元は
/// この待機の成否で処理を変えないため、結果は返さない。
fn await_peer_close(stream: &PipeStream) {
    let deadline = Instant::now() + REQUEST_IDLE_TIMEOUT;
    loop {
        match stream.read_frame(deadline) {
            Ok(Some(_)) => continue,
            Ok(None) => return,
            Err(e) => {
                tracing::debug!("切断待ちを終了しました: {e}");
                return;
            }
        }
    }
}

/// 要求 1 件に対して採用する期限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestDeadline {
    /// 期限内。値は応答送信に使う単調時計上の期限。
    Within(Instant),
    /// 要求を受け取った時点で既に期限を超過している。
    Exceeded,
}

/// 要求の `deadline_unix_ms` とサーバー側上限から、実際に採用する期限を決める。
///
/// 採用するのは両者の短い方であり、`deadline_unix_ms` が未指定の要求には
/// サーバー側上限だけを適用する。
///
/// `deadline_unix_ms` は壁時計（Unix epoch ミリ秒）基準で、`now` の単調時計とは
/// 基準が異なる。そのため `now_unix_ms` との差から残り時間を求め、それを `now` へ
/// 加算して単調時計上の期限に直す。
///
/// 壁時計は時刻調整で前後し得るため、極端な値は次のように扱う。
/// - 遠い未来: サーバー側上限との短い方を採るため、上限を超えて待つことはない。
/// - 過去: 期限超過として扱う。要求元と本プロセスは同一ホストの同一壁時計を参照する
///   ので、往復のミリ秒の間に過去へ回るのは時刻調整に限られる。その場合も要求は
///   未実行のまま中止され副作用が残らず、再試行可能なエラーとして通知できる。
fn resolve_request_deadline(
    now: Instant,
    now_unix_ms: i64,
    server_limit: Duration,
    deadline_unix_ms: Option<u64>,
) -> RequestDeadline {
    let Some(deadline_unix_ms) = deadline_unix_ms else {
        return RequestDeadline::Within(now + server_limit);
    };

    let remaining_ms = i128::from(deadline_unix_ms) - i128::from(now_unix_ms);
    if remaining_ms <= 0 {
        return RequestDeadline::Exceeded;
    }

    // 上限との短い方を採るため、表現できない大きさは上限へ丸めて差し支えない。
    let remaining = Duration::from_millis(u64::try_from(remaining_ms).unwrap_or(u64::MAX));
    RequestDeadline::Within(now + remaining.min(server_limit))
}

/// 要求の `protocol_version` を交渉結果と照合した結果。
#[derive(Debug, PartialEq, Eq)]
enum VersionCheck {
    /// MAJOR 一致かつ MINOR が交渉結果以下。
    Compatible,
    /// MAJOR は一致するが MINOR が交渉結果を超えている。
    MinorTooHigh,
    /// MAJOR が一致しない。
    MajorMismatch,
}

/// 要求の `protocol_version` が交渉結果と互換かを判定する。
fn classify_version(negotiated: ProtocolVersion, requested: ProtocolVersion) -> VersionCheck {
    if requested.major != negotiated.major {
        VersionCheck::MajorMismatch
    } else if requested.minor > negotiated.minor {
        VersionCheck::MinorTooHigh
    } else {
        VersionCheck::Compatible
    }
}

fn read_frame_as<T>(stream: &PipeStream, deadline: Instant) -> Result<T>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let body = stream
        .read_frame(deadline)
        .context("フレームの受信に失敗しました")?
        .context("接続が閉じられました")?;
    let value = deserialize_json(&body)
        .map_err(|e| anyhow::anyhow!("JSON のデコードに失敗しました: {e}"))?;
    Ok(value)
}

/// 応答を `deadline` までに送信する。
fn send_response(
    stream: &PipeStream,
    response: &ResponseEnvelope,
    deadline: Instant,
) -> Result<()> {
    let body = serde_json::to_vec(response).context("応答の JSON 直列化に失敗しました")?;
    stream
        .write_frame(&body, deadline)
        .context("応答の送信に失敗しました")?;
    Ok(())
}

/// 要求の処理結果を応答 Envelope へ載せる。
fn response_envelope(
    protocol_version: ProtocolVersion,
    request_id: RequestId,
    instance_id: InstanceId,
    outcome: Result<serde_json::Value, ErrorObject>,
) -> ResponseEnvelope {
    ResponseEnvelope {
        kind: ResponseKind::Response,
        protocol_version,
        request_id,
        instance_id,
        result: match outcome {
            Ok(result) => ResponseResult::Ok { result },
            Err(error) => ResponseResult::Err { error },
        },
    }
}

/// エラー応答を送信する。
///
/// 送信の期限には要求の deadline ではなくサーバー側上限を使う。期限超過を伝える
/// 応答まで当の期限で打ち切ると、クライアントは理由を得られないまま切断だけを
/// 観測することになる。期限を引数で受け取らないことで、この規則を呼び出し側が
/// 崩せないようにしている。
fn send_error(
    stream: &PipeStream,
    protocol_version: ProtocolVersion,
    request_id: RequestId,
    instance_id: InstanceId,
    error: ErrorObject,
) -> Result<()> {
    let response = response_envelope(protocol_version, request_id, instance_id, Err(error));
    send_response(stream, &response, Instant::now() + WRITE_TIMEOUT)
}

/// エラーコードから既定の再試行可否を採ってエラーを組み立てる。
///
/// 相関 ID は付与しない。応答 Envelope が要求の `request_id` をそのまま返すため、
/// この層の要求と応答は既に対応付けられる。複数の要求元をまたぐ相関は、要求を
/// 発行した側が自身の識別子で付与する。
fn error_object(code: ErrorCode, message: impl Into<String>) -> ErrorObject {
    let retryable = code.default_retryable();
    ErrorObject::new(code, message, retryable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read::Snapshot;
    use aviutl2_mcp_core::{
        AvailableEffect, AvailableEffectItem, Cursor, DisplayRange, EditInfo, EffectFlags,
        EffectItemType, EffectType, Extent, FiniteF64, FrameRange, LayerInfo, ObjectDetail,
        ObjectFilter, ObjectFingerprintInput, ObjectSelector, ObjectSummary, SceneInfo,
        SectionRange,
    };
    use std::sync::Mutex;

    const NEGOTIATED: ProtocolVersion = ProtocolVersion { major: 1, minor: 3 };

    #[test]
    fn compatible_when_same_major_and_minor_within_negotiated() {
        for minor in 0..=3 {
            assert_eq!(
                classify_version(NEGOTIATED, ProtocolVersion { major: 1, minor }),
                VersionCheck::Compatible
            );
        }
    }

    #[test]
    fn minor_above_negotiated_is_rejected() {
        assert_eq!(
            classify_version(NEGOTIATED, ProtocolVersion { major: 1, minor: 4 }),
            VersionCheck::MinorTooHigh
        );
    }

    #[test]
    fn major_mismatch_is_rejected() {
        assert_eq!(
            classify_version(NEGOTIATED, ProtocolVersion { major: 2, minor: 3 }),
            VersionCheck::MajorMismatch
        );
        assert_eq!(
            classify_version(NEGOTIATED, ProtocolVersion { major: 0, minor: 0 }),
            VersionCheck::MajorMismatch
        );
    }

    /// 期限判定の基準時刻。壁時計・単調時計いずれの絶対値にも依存しない。
    const NOW_UNIX_MS: i64 = 1_785_144_000_000;
    const SERVER_LIMIT: Duration = Duration::from_secs(5);

    #[test]
    fn deadline_shorter_than_server_limit_is_adopted() {
        let now = Instant::now();
        assert_eq!(
            resolve_request_deadline(
                now,
                NOW_UNIX_MS,
                SERVER_LIMIT,
                Some((NOW_UNIX_MS + 500) as u64),
            ),
            RequestDeadline::Within(now + Duration::from_millis(500))
        );
    }

    #[test]
    fn server_limit_is_adopted_when_deadline_is_longer() {
        let now = Instant::now();
        assert_eq!(
            resolve_request_deadline(
                now,
                NOW_UNIX_MS,
                SERVER_LIMIT,
                Some((NOW_UNIX_MS + 60_000) as u64),
            ),
            RequestDeadline::Within(now + SERVER_LIMIT)
        );
    }

    #[test]
    fn absent_deadline_uses_server_limit() {
        let now = Instant::now();
        assert_eq!(
            resolve_request_deadline(now, NOW_UNIX_MS, SERVER_LIMIT, None),
            RequestDeadline::Within(now + SERVER_LIMIT)
        );
    }

    #[test]
    fn passed_deadline_is_exceeded() {
        let now = Instant::now();
        for deadline_unix_ms in [NOW_UNIX_MS - 1, NOW_UNIX_MS] {
            assert_eq!(
                resolve_request_deadline(
                    now,
                    NOW_UNIX_MS,
                    SERVER_LIMIT,
                    Some(deadline_unix_ms as u64),
                ),
                RequestDeadline::Exceeded,
                "deadline {deadline_unix_ms} が期限超過として扱われていません"
            );
        }
    }

    #[test]
    fn far_past_deadline_is_exceeded() {
        let now = Instant::now();
        assert_eq!(
            resolve_request_deadline(now, NOW_UNIX_MS, SERVER_LIMIT, Some(0)),
            RequestDeadline::Exceeded
        );
    }

    #[test]
    fn far_future_deadline_is_capped_by_server_limit() {
        let now = Instant::now();
        assert_eq!(
            resolve_request_deadline(now, NOW_UNIX_MS, SERVER_LIMIT, Some(u64::MAX)),
            RequestDeadline::Within(now + SERVER_LIMIT)
        );
    }

    /// テストで用いるプロジェクトの epoch。
    const EPOCH: &str = "9d0a5f4e-2f47-4a13-9a5e-1e2f3a4b5c6d";

    /// テストで用いる現在シーンの ID。
    const SCENE_ID: i32 = 0;

    /// 読み取り口が返す列挙時点の revision。
    const REVISION: u64 = 7;

    /// 読み取り口の代わりに定型データを返す実装。
    ///
    /// 呼ばれた operation を記録するため、受付判定や params の検証で弾かれた
    /// 要求が読み取りへ進んでいないことを確かめられる。
    struct FakeAdapter {
        calls: Mutex<Vec<&'static str>>,
        /// 最初の呼び出しで返す失敗。
        failure: Mutex<Option<ReadError>>,
    }

    impl FakeAdapter {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                failure: Mutex::new(None),
            }
        }

        /// 最初の読み取りが指定の失敗を返す読み取り口を作る。
        fn failing(error: ReadError) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                failure: Mutex::new(Some(error)),
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        /// 呼び出しを記録し、設定された失敗があればそれを返す。
        fn enter(&self, call: &'static str) -> Result<(), ReadError> {
            self.calls.lock().unwrap().push(call);
            match self.failure.lock().unwrap().take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
    }

    impl ReadAdapter for FakeAdapter {
        fn get_edit_info(&self) -> Result<EditInfo, ReadError> {
            self.enter("get_edit_info")?;
            Ok(fake_edit_info())
        }

        fn get_current_scene(&self) -> Result<(SceneInfo, u64), ReadError> {
            self.enter("get_current_scene")?;
            Ok((fake_scene(), REVISION))
        }

        fn list_layers(&self, expected_scene_id: i32) -> Result<Snapshot<LayerInfo>, ReadError> {
            self.enter("list_layers")?;
            ensure_scene(expected_scene_id)?;
            Ok(Snapshot {
                items: fake_layers(),
                snapshot_revision: REVISION,
            })
        }

        fn list_objects(
            &self,
            expected_scene_id: i32,
            filter: Option<&ObjectFilter>,
        ) -> Result<Snapshot<ObjectSummary>, ReadError> {
            self.enter("list_objects")?;
            ensure_scene(expected_scene_id)?;
            let layer_min = filter.and_then(|filter| filter.layer_min).unwrap_or(0);
            let items = fake_objects()
                .into_iter()
                .filter(|object| object.layer >= layer_min)
                .collect();
            Ok(Snapshot {
                items,
                snapshot_revision: REVISION,
            })
        }

        fn get_object(&self, selector: &ObjectSelector) -> Result<ObjectDetail, ReadError> {
            self.enter("get_object")?;
            let summary = fake_object();
            if *selector != summary.selector {
                return Err(ReadError::ObjectNotFound);
            }
            Ok(ObjectDetail {
                alias: "[1:100]".to_string(),
                sections: vec![SectionRange {
                    start: 100,
                    end: 200,
                }],
                effects: Vec::new(),
                project_revision: REVISION,
                summary,
            })
        }

        fn list_available_effects(
            &self,
            effect_type: Option<&EffectType>,
        ) -> Result<Snapshot<AvailableEffect>, ReadError> {
            self.enter("list_available_effects")?;
            let mut items = fake_effects();
            if let Some(effect_type) = effect_type {
                items.retain(|effect| effect.effect_type == *effect_type);
            }
            Ok(Snapshot {
                items,
                snapshot_revision: REVISION,
            })
        }
    }

    fn ensure_scene(expected_scene_id: i32) -> Result<(), ReadError> {
        if expected_scene_id == SCENE_ID {
            Ok(())
        } else {
            Err(ReadError::SceneMismatch {
                expected: expected_scene_id,
                current: SCENE_ID,
            })
        }
    }

    fn fake_scene() -> SceneInfo {
        SceneInfo {
            id: SCENE_ID,
            name: Some("Scene 1".to_string()),
            width: 1920,
            height: 1080,
            fps: FiniteF64::try_new(60.0),
            fps_rate: 60,
            fps_scale: 1,
            sample_rate: 48000,
        }
    }

    fn fake_edit_info() -> EditInfo {
        EditInfo {
            scene: fake_scene(),
            cursor: Cursor {
                frame: 12,
                layer: 1,
            },
            extent: Extent {
                frame_max: 3600,
                layer_max: 2,
            },
            display: DisplayRange {
                frame_start: 0,
                layer_start: 0,
                frame_num: 600,
                layer_num: 10,
            },
            selected_range: Some(FrameRange { start: 10, end: 20 }),
            grid_bpm: Vec::new(),
            project_epoch: EPOCH.to_string(),
            project_revision: REVISION,
        }
    }

    fn fake_layers() -> Vec<LayerInfo> {
        (0..3)
            .map(|index| LayerInfo {
                index,
                name: Some(format!("レイヤー {index}")),
                enabled: true,
                locked: false,
                object_count: 1,
            })
            .collect()
    }

    /// レイヤー 1・フレーム 100 のオブジェクト。
    fn fake_object() -> ObjectSummary {
        ObjectSummary::new(
            EPOCH,
            ObjectFingerprintInput {
                scene_id: SCENE_ID,
                layer: 1,
                frame_start: 100,
                frame_end: 200,
                name: Some("立ち絵"),
                alias: "[1:100]",
            },
        )
    }

    fn fake_objects() -> Vec<ObjectSummary> {
        vec![
            ObjectSummary::new(
                EPOCH,
                ObjectFingerprintInput {
                    scene_id: SCENE_ID,
                    layer: 0,
                    frame_start: 0,
                    frame_end: 99,
                    name: None,
                    alias: "[0:0]",
                },
            ),
            fake_object(),
        ]
    }

    fn fake_effects() -> Vec<AvailableEffect> {
        vec![
            AvailableEffect {
                name: "ぼかし".to_string(),
                effect_type: EffectType::Filter,
                flags: EffectFlags::from_raw(1),
                items: vec![AvailableEffectItem {
                    name: "範囲".to_string(),
                    item_type: EffectItemType::Integer,
                }],
            },
            AvailableEffect {
                name: "動画ファイル".to_string(),
                effect_type: EffectType::Input,
                flags: EffectFlags::from_raw(3),
                items: Vec::new(),
            },
        ]
    }

    /// 受付可能な状態・期限内で読み取りを実行する。
    fn read(
        adapter: &FakeAdapter,
        operation: ReadOperation,
        params: Value,
    ) -> Result<Value, ErrorObject> {
        execute_read(
            adapter,
            &InstanceState::Ready,
            operation,
            &params,
            RequestDeadline::Within(Instant::now() + READ_TIMEOUT),
        )
    }

    /// 全 operation と、その operation が受け付ける最小の params。
    fn all_operations() -> Vec<(ReadOperation, Value)> {
        vec![
            (ReadOperation::GetEditInfo, json!({})),
            (ReadOperation::GetCurrentScene, json!({})),
            (
                ReadOperation::ListLayers,
                json!({ "expected_scene_id": SCENE_ID }),
            ),
            (
                ReadOperation::ListObjects,
                json!({ "expected_scene_id": SCENE_ID }),
            ),
            (
                ReadOperation::GetObject,
                json!({ "selector": fake_object().selector }),
            ),
            (ReadOperation::ListAvailableEffects, json!({})),
        ]
    }

    #[test]
    fn known_operations_are_routed() {
        assert_eq!(classify_operation("ping").unwrap(), Operation::Ping);
        for (name, operation) in [
            ("get_edit_info", ReadOperation::GetEditInfo),
            ("get_current_scene", ReadOperation::GetCurrentScene),
            ("list_layers", ReadOperation::ListLayers),
            ("list_objects", ReadOperation::ListObjects),
            ("get_object", ReadOperation::GetObject),
            (
                "list_available_effects",
                ReadOperation::ListAvailableEffects,
            ),
        ] {
            assert_eq!(
                classify_operation(name).unwrap(),
                Operation::Read(operation),
                "{name} が読み取りへ振り分けられていません"
            );
        }
    }

    #[test]
    fn unknown_operation_is_unsupported() {
        for name in ["", "Ping", "create_object", "list_layer"] {
            let error = classify_operation(name).unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::UnsupportedOperation,
                "{name} が受理されました"
            );
            assert!(!error.retryable);
        }
    }

    #[test]
    fn get_edit_info_returns_edit_info() {
        let adapter = FakeAdapter::new();
        let result = read(&adapter, ReadOperation::GetEditInfo, json!({})).unwrap();

        assert_eq!(result["scene"]["id"], SCENE_ID);
        assert_eq!(result["scene"]["name"], "Scene 1");
        assert_eq!(result["project_epoch"], EPOCH);
        assert_eq!(result["project_revision"], REVISION);
        assert_eq!(adapter.calls(), vec!["get_edit_info"]);
    }

    #[test]
    fn get_current_scene_returns_scene_and_revision() {
        let adapter = FakeAdapter::new();
        let result = read(&adapter, ReadOperation::GetCurrentScene, json!({})).unwrap();

        assert_eq!(result["scene"]["id"], SCENE_ID);
        assert_eq!(result["project_revision"], REVISION);
        assert_eq!(adapter.calls(), vec!["get_current_scene"]);
    }

    #[test]
    fn list_layers_returns_requested_page() {
        let adapter = FakeAdapter::new();
        let result = read(
            &adapter,
            ReadOperation::ListLayers,
            json!({ "expected_scene_id": SCENE_ID, "offset": 1, "limit": 1 }),
        )
        .unwrap();

        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        assert_eq!(result["items"][0]["index"], 1);
        assert_eq!(result["page"]["total_count"], 3);
        assert_eq!(result["page"]["count"], 1);
        assert_eq!(result["page"]["offset"], 1);
        assert_eq!(result["page"]["has_more"], true);
        assert_eq!(result["page"]["next_offset"], 2);
        assert_eq!(result["page"]["snapshot_revision"], REVISION);
    }

    #[test]
    fn list_objects_passes_filter_to_the_adapter() {
        let adapter = FakeAdapter::new();
        let result = read(
            &adapter,
            ReadOperation::ListObjects,
            json!({ "expected_scene_id": SCENE_ID, "filter": { "layer_min": 1 } }),
        )
        .unwrap();

        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        assert_eq!(result["items"][0]["layer"], 1);
        assert_eq!(result["page"]["total_count"], 1);
        assert_eq!(result["page"]["snapshot_revision"], REVISION);
    }

    #[test]
    fn get_object_passes_selector_to_the_adapter() {
        let adapter = FakeAdapter::new();
        let selector = fake_object().selector;
        let result = read(
            &adapter,
            ReadOperation::GetObject,
            json!({ "selector": selector }),
        )
        .unwrap();

        assert_eq!(result["summary"]["layer"], 1);
        assert_eq!(result["summary"]["frame_start"], 100);
        assert_eq!(result["summary"]["selector"], json!(selector));
        assert_eq!(result["project_revision"], REVISION);
    }

    #[test]
    fn list_available_effects_filters_by_type() {
        let adapter = FakeAdapter::new();
        let result = read(
            &adapter,
            ReadOperation::ListAvailableEffects,
            json!({ "effect_type": "input" }),
        )
        .unwrap();

        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        assert_eq!(result["items"][0]["name"], "動画ファイル");
        assert_eq!(result["page"]["total_count"], 1);
    }

    #[test]
    fn unknown_params_field_is_invalid_argument() {
        for (operation, params) in all_operations() {
            let mut params = params;
            params
                .as_object_mut()
                .unwrap()
                .insert("future".to_string(), json!(1));
            let adapter = FakeAdapter::new();

            let error = read(&adapter, operation, params).unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::InvalidArgument,
                "{operation:?} が未知フィールドを受理しました"
            );
            assert!(
                adapter.calls().is_empty(),
                "{operation:?} が未知フィールドのまま読み取りへ進みました"
            );
        }
    }

    #[test]
    fn malformed_params_are_invalid_argument() {
        let cases = [
            (ReadOperation::ListLayers, json!({})),
            (
                ReadOperation::ListLayers,
                json!({ "expected_scene_id": "0" }),
            ),
            (
                ReadOperation::ListObjects,
                json!({ "expected_scene_id": SCENE_ID, "filter": { "layer_min": -1 } }),
            ),
            (ReadOperation::GetObject, json!({})),
            (
                ReadOperation::ListAvailableEffects,
                json!({ "effect_type": 1 }),
            ),
        ];

        for (operation, params) in cases {
            let adapter = FakeAdapter::new();
            let error = read(&adapter, operation, params.clone()).unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::InvalidArgument,
                "{operation:?} が {params} を受理しました"
            );
            assert!(adapter.calls().is_empty(), "{operation:?}: {params}");
        }
    }

    #[test]
    fn limit_out_of_range_is_invalid_argument_without_reading() {
        let paged = [
            (
                ReadOperation::ListLayers,
                json!({ "expected_scene_id": SCENE_ID }),
            ),
            (
                ReadOperation::ListObjects,
                json!({ "expected_scene_id": SCENE_ID }),
            ),
            (ReadOperation::ListAvailableEffects, json!({})),
        ];

        for (operation, params) in paged {
            for limit in [0, 201] {
                let mut params = params.clone();
                params
                    .as_object_mut()
                    .unwrap()
                    .insert("limit".to_string(), json!(limit));
                let adapter = FakeAdapter::new();

                let error = read(&adapter, operation, params).unwrap_err();
                assert_eq!(
                    error.code,
                    ErrorCode::InvalidArgument,
                    "{operation:?} が limit {limit} を受理しました"
                );
                assert!(
                    adapter.calls().is_empty(),
                    "{operation:?} が limit {limit} のまま読み取りへ進みました"
                );
            }
        }
    }

    #[test]
    fn inverted_layer_filter_is_invalid_argument_without_reading() {
        let adapter = FakeAdapter::new();
        let error = read(
            &adapter,
            ReadOperation::ListObjects,
            json!({
                "expected_scene_id": SCENE_ID,
                "filter": { "layer_min": 2, "layer_max": 1 },
            }),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(
            adapter.calls().is_empty(),
            "逆転した絞り込み条件のまま読み取りへ進みました"
        );
    }

    #[test]
    fn snapshot_revision_mismatch_is_precondition_failed() {
        let paged = [
            (
                ReadOperation::ListLayers,
                json!({ "expected_scene_id": SCENE_ID, "snapshot_revision": REVISION - 1 }),
            ),
            (
                ReadOperation::ListObjects,
                json!({ "expected_scene_id": SCENE_ID, "snapshot_revision": REVISION - 1 }),
            ),
        ];

        for (operation, params) in paged {
            let adapter = FakeAdapter::new();
            let error = read(&adapter, operation, params).unwrap_err();

            assert_eq!(
                error.code,
                ErrorCode::PreconditionFailed,
                "{operation:?} が古い snapshot_revision を受理しました"
            );
            assert!(error.retryable);
            assert_eq!(error.details["requested_snapshot_revision"], REVISION - 1);
            assert_eq!(error.details["current_snapshot_revision"], REVISION);
        }
    }

    #[test]
    fn effect_catalog_page_ignores_snapshot_revision() {
        // 登録済み effect の一覧はプロジェクトの編集内容から独立しており、
        // revision の照合対象にしない。無関係な編集で revision が進んでも
        // 後続ページは拒否されない。
        let adapter = FakeAdapter::new();
        let result = read(
            &adapter,
            ReadOperation::ListAvailableEffects,
            json!({ "snapshot_revision": REVISION - 1 }),
        )
        .unwrap();

        assert_eq!(result["items"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn effect_catalog_page_reports_the_revision_of_the_enumeration() {
        // 照合しないことと、ページのメタ情報へ何を載せるかは別である。0 のような
        // 固定値は実在し得る revision と区別が付かず、他の一覧から得た値と混同
        // され得るため、列挙時点の revision をそのまま載せる。
        let adapter = FakeAdapter::new();
        let result = read(&adapter, ReadOperation::ListAvailableEffects, json!({})).unwrap();

        assert_eq!(result["page"]["snapshot_revision"], 7);
        assert_eq!(result["page"]["snapshot_revision"], REVISION);
    }

    #[test]
    fn starting_rejects_read_without_touching_the_adapter() {
        for (operation, params) in all_operations() {
            let adapter = FakeAdapter::new();
            let error = execute_read(
                &adapter,
                &InstanceState::Starting,
                operation,
                &params,
                RequestDeadline::Within(Instant::now() + READ_TIMEOUT),
            )
            .unwrap_err();

            assert_eq!(
                error.code,
                ErrorCode::HostBusy,
                "{operation:?} が起動処理中に受理されました"
            );
            assert!(error.retryable);
            assert_eq!(error.details["retry_after_ms"], 500);
            assert!(
                adapter.calls().is_empty(),
                "{operation:?} が起動処理中に読み取り口を呼びました"
            );
        }
    }

    #[test]
    fn timeouts_match_the_intended_budget() {
        // 期限と再試行案内の設計値。変えると要求元との取り決めが変わるため、
        // 値そのものを主張する。
        assert_eq!(READ_TIMEOUT, Duration::from_secs(5));
        assert_eq!(WRITE_TIMEOUT, Duration::from_secs(5));
        assert_eq!(HOST_BUSY_RETRY_AFTER_MS, 500);
    }

    #[test]
    fn admit_read_accepts_only_serviceable_states() {
        for state in [InstanceState::Ready, InstanceState::Busy] {
            assert_eq!(admit_read(&state), Ok(()), "{state} が拒否されました");
        }

        for state in [
            InstanceState::Starting,
            InstanceState::Draining,
            InstanceState::Unknown("future".to_string()),
        ] {
            let error = admit_read(&state).unwrap_err();
            assert_eq!(error.code, ErrorCode::HostBusy, "{state} が受理されました");
            assert!(error.retryable);
            assert_eq!(error.details["retry_after_ms"], 500);
        }
    }

    #[test]
    fn gone_instance_is_not_advised_to_retry() {
        // 終了済みのインスタンスは同じ相手として戻らない。再試行の間隔を案内すると
        // 待てば復活するかのように読める。
        let error = admit_read(&InstanceState::Gone).unwrap_err();
        assert_eq!(error.code, ErrorCode::InstanceStale);
        assert_eq!(error.details.get("retry_after_ms"), None);
    }

    /// 読み取りの失敗の全 variant。新しい variant を足したらここへも足す。
    fn read_error_variants() -> Vec<fn() -> ReadError> {
        vec![
            || ReadError::NotReady,
            || ReadError::EditBlocked {
                state: crate::read::EditState::Preview,
            },
            || ReadError::EditBlocked {
                state: crate::read::EditState::Save,
            },
            || ReadError::SceneMismatch {
                expected: 3,
                current: SCENE_ID,
            },
            || ReadError::EpochMismatch,
            || ReadError::FingerprintAlgorithmMismatch {
                requested: "sha256-future-v9".to_string(),
                supported: "sha256-alias-v1".to_string(),
            },
            || ReadError::FingerprintMismatch,
            || ReadError::ObjectNotFound,
            || ReadError::AmbiguousObject { candidate_count: 2 },
            || ReadError::InvalidFilter(ObjectFilterError::InvertedLayerRange { min: 8, max: 1 }),
            || ReadError::Sdk {
                operation: "call_read_section",
            },
            || ReadError::Panicked,
        ]
    }

    #[test]
    fn read_failures_keep_their_code_and_details() {
        for make in read_error_variants() {
            let expected = make();
            let adapter = FakeAdapter::failing(make());

            let error = read(&adapter, ReadOperation::GetEditInfo, json!({})).unwrap_err();

            assert_eq!(error.code, expected.error_code(), "{expected}");
            assert_eq!(error.retryable, expected.retryable(), "{expected}");
            assert_eq!(error.message, expected.to_string());
            // 再試行間隔は補助情報の中だけに現れ、重ねて載せない。
            assert_eq!(error.details, expected.details(), "{expected}");
            assert_eq!(
                error.details.get("retry_after_ms").and_then(Value::as_u64),
                expected.retry_after_ms(),
                "{expected}"
            );
        }
    }

    #[test]
    fn scene_mismatch_from_the_adapter_is_precondition_failed() {
        let adapter = FakeAdapter::new();
        let error = read(
            &adapter,
            ReadOperation::ListLayers,
            json!({ "expected_scene_id": SCENE_ID + 1 }),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::PreconditionFailed);
        assert_eq!(error.details["expected_scene_id"], SCENE_ID + 1);
        assert_eq!(error.details["current_scene_id"], SCENE_ID);
    }

    #[test]
    fn responses_do_not_expose_handles() {
        let mut documents = Vec::new();
        for (operation, params) in all_operations() {
            let adapter = FakeAdapter::new();
            let result = read(&adapter, operation, params).unwrap();
            documents.push(serde_json::to_string(&result).unwrap());
        }
        for make in read_error_variants() {
            let adapter = FakeAdapter::failing(make());
            let error = read(&adapter, ReadOperation::GetEditInfo, json!({})).unwrap_err();
            documents.push(serde_json::to_string(&error).unwrap());
        }

        for document in documents {
            let lowered = document.to_lowercase();
            for forbidden in ["handle", "pointer", "0x", "secret", "nonce"] {
                assert!(
                    !lowered.contains(forbidden),
                    "{forbidden} が応答に含まれます: {document}"
                );
            }
        }
    }

    #[test]
    fn exceeded_deadline_skips_the_read() {
        for (operation, params) in all_operations() {
            let adapter = FakeAdapter::new();
            let error = execute_read(
                &adapter,
                &InstanceState::Ready,
                operation,
                &params,
                RequestDeadline::Exceeded,
            )
            .unwrap_err();

            assert_eq!(
                error.code,
                ErrorCode::Timeout,
                "{operation:?} が期限超過後に実行されました"
            );
            assert!(error.retryable);
            assert!(
                adapter.calls().is_empty(),
                "{operation:?} が期限超過後に読み取り口を呼びました"
            );
        }
    }

    #[test]
    fn send_uses_the_remaining_budget_after_the_read() {
        let now = Instant::now();
        // 読み取りに 1 秒使い、要求の残りは 3 秒。送信上限より短いので残りを採る。
        assert_eq!(
            decide_send(
                now,
                NOW_UNIX_MS,
                RequestDeadline::Within(now + Duration::from_secs(4)),
                Some((NOW_UNIX_MS + 3_000) as u64),
            ),
            SendDecision::Send(now + Duration::from_secs(3))
        );
    }

    #[test]
    fn send_is_capped_by_the_write_limit() {
        let now = Instant::now();
        assert_eq!(
            decide_send(
                now,
                NOW_UNIX_MS,
                RequestDeadline::Within(now + Duration::from_secs(4)),
                None,
            ),
            SendDecision::Send(now + WRITE_TIMEOUT)
        );
        assert_eq!(
            decide_send(
                now,
                NOW_UNIX_MS,
                RequestDeadline::Within(now + Duration::from_secs(4)),
                Some((NOW_UNIX_MS + 60_000) as u64),
            ),
            SendDecision::Send(now + WRITE_TIMEOUT)
        );
    }

    #[test]
    fn result_is_discarded_when_the_read_used_up_its_deadline() {
        let now = Instant::now();
        for read_deadline in [now, now - Duration::from_millis(1)] {
            assert_eq!(
                decide_send(
                    now,
                    NOW_UNIX_MS,
                    RequestDeadline::Within(read_deadline),
                    None,
                ),
                SendDecision::Discard
            );
        }
    }

    #[test]
    fn result_is_discarded_when_the_request_deadline_passed_during_the_read() {
        let now = Instant::now();
        assert_eq!(
            decide_send(
                now,
                NOW_UNIX_MS,
                RequestDeadline::Within(now + Duration::from_secs(4)),
                Some(NOW_UNIX_MS as u64),
            ),
            SendDecision::Discard
        );
    }

    #[test]
    fn unstarted_read_still_gets_a_send_budget() {
        // 実行前に期限を超過していた要求は捨てる結果を持たない。理由を返せるよう
        // 送信上限だけで送る。
        let now = Instant::now();
        assert_eq!(
            decide_send(now, NOW_UNIX_MS, RequestDeadline::Exceeded, Some(0)),
            SendDecision::Send(now + WRITE_TIMEOUT)
        );
    }

    /// 読み取りの期限を使い切った状態。結果は捨てる判定になる。
    fn spent_read_deadline(now: Instant) -> RequestDeadline {
        RequestDeadline::Within(now - Duration::from_millis(1))
    }

    #[test]
    fn successful_result_is_replaced_by_timeout_when_discarded() {
        let now = Instant::now();
        let response = resolve_read_response(
            now,
            NOW_UNIX_MS,
            spent_read_deadline(now),
            None,
            Ok(json!({ "scene": { "id": SCENE_ID } })),
        );

        let error = response.outcome.unwrap_err();
        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(error.retryable);
        assert!(response.discarded);
        assert_eq!(response.deadline, now + WRITE_TIMEOUT);
    }

    #[test]
    fn failure_keeps_its_reason_when_the_deadline_passed() {
        // 読み取りが失敗していれば捨てる結果は無い。期限超過で上書きすると、
        // 再試行しても解消しない理由が再試行可能な timeout に化ける。
        let now = Instant::now();
        for original in [
            error_object(ErrorCode::InvalidArgument, "params の解釈に失敗しました"),
            error_object(ErrorCode::HostBusy, "起動処理中です"),
            error_object(ErrorCode::EditBlocked, "再生中です"),
        ] {
            let response = resolve_read_response(
                now,
                NOW_UNIX_MS,
                spent_read_deadline(now),
                None,
                Err(original.clone()),
            );

            let error = response.outcome.unwrap_err();
            assert_eq!(error, original, "失敗の理由が書き換わりました");
            assert!(
                !response.discarded,
                "捨てる結果が無いのに破棄として扱われました"
            );
            assert_eq!(response.deadline, now + WRITE_TIMEOUT);
        }
    }

    #[test]
    fn outcome_is_kept_within_the_deadline() {
        let now = Instant::now();
        let result = json!({ "items": [] });
        let response = resolve_read_response(
            now,
            NOW_UNIX_MS,
            RequestDeadline::Within(now + Duration::from_secs(4)),
            Some((NOW_UNIX_MS + 3_000) as u64),
            Ok(result.clone()),
        );

        assert_eq!(response.outcome.unwrap(), result);
        assert!(!response.discarded);
        assert_eq!(response.deadline, now + Duration::from_secs(3));
    }
}
