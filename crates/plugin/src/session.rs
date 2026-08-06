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

use crate::edit::{EditAdapter, EditError};
use crate::lifecycle::Lifecycle;
use crate::pipe::PipeStream;
use crate::read::{ReadAdapter, ReadError};
use crate::render::{RenderAdapter, RenderError};
use anyhow::{Context, Result};
use aviutl2_mcp_core::{
    AddEffectParams, ApplyBatchParams, BatchInputError, ClientAuth, ClientHello,
    CreateObjectParams, CreateObjectSectionParams, DeleteEffectParams, DeleteObjectParams,
    DeleteObjectSectionParams, EditInputError, EditOperation, EffectItemValuesInputError,
    ErrorCode, ErrorObject, GetCurrentSceneParams, GetCurrentSceneResult, GetEditInfoParams,
    GetEffectItemValuesParams, GetObjectParams, GetSelectionParams, InstanceId, InstanceState,
    KnownOperation, LimitOutOfRange, ListAvailableEffectsParams, ListAvailableEffectsResult,
    ListFontsParams, ListFontsResult, ListLayersParams, ListLayersResult, ListModulesParams,
    ListModulesResult, ListObjectAliasesParams, ListObjectsParams, ListObjectsResult,
    ListPalettesParams, MoveObjectParams, MoveObjectSectionParams, Nonce, ObjectFilterError,
    PageWindow, PongProject, PongResult, ProtocolVersion, ReadOperation, RenderFrameParams,
    RenderFrameResult, RenderInputError, RenderOperation, RequestEnvelope, RequestId,
    ResponseEnvelope, ResponseKind, ResponseResult, ScaledBudgets, SelectionSnapshot,
    SetEffectEnabledParams, SetGridBpmParams, SetLayerStateParams, SetObjectItemParams,
    SetObjectNameParams, SetSceneSettingsParams, SetSelectionParams, SnapshotRevisionMismatch,
    TextSyntaxError, ValidatedPageRequest, compute_client_mac, compute_server_mac,
    deserialize_json, take_page, take_window, verify_mac,
};
use chrono::Utc;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 現在の設定が定める期限配分。
///
/// 予算 15 種には設定の倍率が掛かる。**倍率は全項へ同じ比で掛かり、採用の前に
/// 不等式を検査してある**ため、ここで引く値は要求元の予算と常に噛み合う。
/// plugin 側で範囲を判定し直さないことが、両端が同じ結論に至る根拠である。
fn budgets() -> ScaledBudgets {
    crate::settings::current().budgets()
}

/// handshake（M1 受信 〜 M3 検証）全体に許す上限。
///
/// handshake は接続確立直後に 3 往復で完結する軽量な処理であり、
/// クライアントの待ち時間は含まない。未応答のクライアントが待受を占有する
/// 時間をこの値に抑える。
///
/// 要求元は接続・handshake・ping をまとめた 1 つの予算で待つため、上限は
/// その予算の内側から配分する。
pub(crate) fn handshake_timeout() -> Duration {
    budgets().plugin_handshake()
}

/// 認証済み接続で次の要求フレームを待つ上限。
///
/// 1 接続は handshake → 要求 → 応答の直列で完結し、クライアントは応答受信後に
/// 切断する。したがってこの待機は実質「相手の切断（EOF）を受け取るまで」であり、
/// 通常はミリ秒で終わる。待受インスタンスは 1 本だけで、1 接続の処理中は
/// 新たな接続を受理できないため、黙り込んだクライアントが占有できる時間を
/// この値に抑える。
///
/// **掛かるのは要求フレームの到着待ちだけであり、要求の処理時間を含まない。**
/// したがって要求フェーズの予算配分には属さず、予算と比べる量でもない。要求元が
/// 解決フェーズを終えてから要求を送り出すまでの間隔を覆えるだけの長さを採る。
///
/// **予算に合わせて引き上げてはならない。** 待受インスタンスは 1 本だけなので、
/// この値はそのまま「要求を送らないクライアントが他の全員を締め出せる時間」で
/// ある。長い予算を持つ operation が増えても、締め出しの許容が伸びる理由には
/// ならない。
const REQUEST_IDLE_TIMEOUT: Duration = Duration::from_secs(15);

/// 1 フレームの送信に許す上限。
///
/// 受信側がバッファを読み出さない場合でも送信側が滞留しないようにする。
/// 要求が deadline を指定した場合は、この上限と deadline の短い方を採用する。
///
/// 実行の上限とは別枠で確保するため、実行が上限を使い切っても応答を送る
/// 持ち時間が残る。
fn write_timeout() -> Duration {
    budgets().plugin_write()
}

/// 読み取りの実行に許す上限。
#[cfg(test)]
fn read_timeout() -> Duration {
    budgets().plugin_execution(aviutl2_mcp_core::RequestBudgetKind::Read)
}

/// 編集の実行に許す上限。
#[cfg(test)]
fn edit_timeout() -> Duration {
    budgets().plugin_execution(aviutl2_mcp_core::RequestBudgetKind::Edit)
}

/// 一括適用の実行に許す上限。
#[cfg(test)]
fn batch_timeout() -> Duration {
    budgets().plugin_execution(aviutl2_mcp_core::RequestBudgetKind::Batch)
}

/// レンダリングの実行に許す上限。
#[cfg(test)]
fn render_timeout() -> Duration {
    budgets().plugin_execution(aviutl2_mcp_core::RequestBudgetKind::Render)
}

/// 読み取りを受け付けられない状態で案内する再試行間隔（ミリ秒）。
///
/// 起動処理も終了処理も利用者の操作を待たずに進むため、待ち時間は短く採る。
const HOST_BUSY_RETRY_AFTER_MS: u64 = 500;

/// 1 接続の処理を panic boundary で包んで実行する。
///
/// 読み取り口・編集口・レンダリングの実行口は全接続で共有し、SDK 呼び出しと
/// プロジェクト状態の参照はその内側へ閉じる。
pub fn handle_connection(
    stream: PipeStream,
    lifecycle: Arc<Lifecycle>,
    read_adapter: Arc<dyn ReadAdapter>,
    edit_adapter: Arc<dyn EditAdapter>,
    render_adapter: Arc<dyn RenderAdapter>,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Err(e) = run_connection(
            &stream,
            &lifecycle,
            read_adapter.as_ref(),
            edit_adapter.as_ref(),
            render_adapter.as_ref(),
        ) {
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
    edit_adapter: &dyn EditAdapter,
    render_adapter: &dyn RenderAdapter,
) -> Result<()> {
    perform_handshake(stream, lifecycle)?;
    run_request_loop(
        stream,
        lifecycle,
        read_adapter,
        edit_adapter,
        render_adapter,
    )
}

/// handshake を実行する。
///
/// 相手が名乗るプロトコルバージョンは [`ProtocolVersion::CURRENT`] との完全一致を
/// 求め、異なる版を名乗る相手は接続ごと拒否する。
///
/// 検証に失敗した場合はエラー応答を返さずに `Err` を返し、呼び出し元が接続を
/// 切断する。未認証の相手へ失敗理由を開示しないため、理由はローカルログにのみ
/// 記録する。`auth_secret`・nonce・MAC はログに出さない。
fn perform_handshake(stream: &PipeStream, lifecycle: &Lifecycle) -> Result<()> {
    let deadline = Instant::now() + handshake_timeout();

    let client_hello = read_frame_as::<ClientHello>(stream, deadline)
        .context("ClientHello の受信に失敗しました")?;

    if client_hello.instance_id != lifecycle.instance_id() {
        anyhow::bail!("ClientHello の instance_id が一致しないため接続を切断します");
    }

    if client_hello.protocol_version != ProtocolVersion::CURRENT {
        anyhow::bail!("プロトコルバージョンが一致しないため接続を切断します");
    }

    let server_nonce = Nonce::generate();
    let server_mac = compute_server_mac(
        lifecycle.auth_secret().as_bytes(),
        &client_hello.client_nonce,
        &server_nonce,
        &lifecycle.instance_id(),
        &ProtocolVersion::CURRENT,
    );

    let server_auth = aviutl2_mcp_core::ServerAuth {
        protocol_version: ProtocolVersion::CURRENT,
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

    Ok(())
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
    edit_adapter: &dyn EditAdapter,
    render_adapter: &dyn RenderAdapter,
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

        // 別のプロセスが書いた設定をここで取り込む。費用は読み取り 1 回で
        // あり、内容が同じなら再解析しない。
        //
        // **要求が届いてから読む。** 到着を待っている間（最大
        // [`REQUEST_IDLE_TIMEOUT`]）に書かれた変更は、待つ前に読んだのでは
        // その要求に効かない。費用は変わらず、契機は「要求 1 件の処理を
        // 始めるとき」に一致する。
        //
        // **設定のために専用のスレッドを持たないのは、要求が来ないときに設定が
        // 古いことを誰も観測しないためである。**
        crate::settings::refresh();

        let request: RequestEnvelope = deserialize_json(&body)
            .map_err(|e| anyhow::anyhow!("RequestEnvelope のデコードに失敗しました: {e}"))?;

        // 版が違えば要求の解釈そのものが保証されず、接続を継続できない。
        // handshake は完了しているため理由を 1 度返し、以降の要求は処理せずに
        // クライアントの切断を待ってから閉じる。
        if request.protocol_version != ProtocolVersion::CURRENT {
            send_error(
                stream,
                request.request_id,
                request.instance_id,
                error_object(
                    ErrorCode::ProtocolMismatch,
                    "要求の protocol_version が一致しません",
                ),
            )?;
            await_peer_close(stream);
            break;
        }

        if request.instance_id != lifecycle.instance_id() {
            send_error(
                stream,
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
                send_error(stream, request.request_id, request.instance_id, error)?;
                continue;
            }
        };

        // 期限は operation の実行に対する制約であり、要求自体の妥当性検証
        // （version・instance・operation）を通した後に評価する。妥当性の誤りは
        // 再試行しても解消しないため、再試行可能な `timeout` より先に返す。
        //
        // 上限は分類した operation から 1 か所で引く。族ごとに引き直すと、
        // 予算区分を足したときに一部の族だけが古い上限のまま残る。
        let execution_deadline = resolve_execution_deadline(
            Instant::now(),
            Utc::now().timestamp_millis(),
            operation,
            request.deadline_unix_ms,
        );

        match operation {
            Operation::Ping => {
                let deadline = match execution_deadline {
                    RequestDeadline::Within(deadline) => deadline,
                    RequestDeadline::Exceeded => {
                        send_error(
                            stream,
                            request.request_id,
                            request.instance_id,
                            timeout_before_execution(),
                        )?;
                        continue;
                    }
                };
                let response = ResponseEnvelope::pong(
                    ProtocolVersion::CURRENT,
                    request.request_id,
                    &pong_result(lifecycle.instance_id(), lifecycle.state(), read_adapter),
                );
                send_response(stream, &response, deadline)?;
            }
            Operation::Read(operation) => {
                let outcome = execute_read(
                    read_adapter,
                    &lifecycle.state(),
                    operation,
                    &request.params,
                    execution_deadline,
                );

                let read_response = resolve_read_response(
                    Instant::now(),
                    Utc::now().timestamp_millis(),
                    execution_deadline,
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
                    request.request_id,
                    lifecycle.instance_id(),
                    read_response.outcome,
                );
                send_response(stream, &response, read_response.deadline)?;
            }
            Operation::Edit(operation) => {
                let outcome = execute_edit(
                    edit_adapter,
                    &lifecycle.state(),
                    operation,
                    &request.params,
                    execution_deadline,
                );

                if let Err(error) = &outcome {
                    tracing::debug!(
                        request_id = ?request.request_id,
                        operation = %request.operation,
                        code = %error.code,
                        "編集要求を失敗として返します"
                    );
                }

                // 期限を過ぎていても結果は捨てず、送信の持ち時間を丸ごと充てる。
                let deadline = retained_send_deadline(Instant::now());
                let response =
                    response_envelope(request.request_id, lifecycle.instance_id(), outcome);
                send_response(stream, &response, deadline)?;
            }
            Operation::Render(operation) => {
                let outcome = execute_render(
                    render_adapter,
                    &lifecycle.state(),
                    operation,
                    &request.params,
                    execution_deadline,
                );

                if let Err(error) = &outcome {
                    tracing::debug!(
                        request_id = ?request.request_id,
                        operation = %request.operation,
                        code = %error.code,
                        "レンダリング要求を失敗として返します"
                    );
                }

                deliver_render_response(render_adapter, outcome, |outcome| {
                    // 期限を過ぎていても結果は捨てず、送信の持ち時間を丸ごと
                    // 充てる。
                    let deadline = retained_send_deadline(Instant::now());
                    let response =
                        response_envelope(request.request_id, lifecycle.instance_id(), outcome);
                    send_response(stream, &response, deadline)
                })?;
            }
        }
    }

    Ok(())
}

/// 生存確認の応答内容を組み立てる。
///
/// プロジェクトの状態はライフサイクル状態に関わらず載せる。読み取り口が返すのは
/// SDK に触れずに読める値だけであり、起動処理中でも参照できる。要求元はこの値で
/// インスタンス一覧の project を埋めるため、載せられるものを落とさない。
///
/// 現在シーンは載せない。シーン ID もシーン名も編集ハンドルを介してしか読めず、
/// 生存確認を受け付けるすべての状態でそれを呼べるとは限らない。
fn pong_result(
    instance_id: InstanceId,
    state: InstanceState,
    read_adapter: &dyn ReadAdapter,
) -> PongResult {
    let status = read_adapter.project_status();
    PongResult::new(instance_id, state).with_project(PongProject {
        epoch: status.epoch,
        revision: status.revision,
        modified: status.modified,
    })
}

/// 受理できる operation。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    /// 生存確認。ライフサイクル状態を問わず受け付ける。
    Ping,
    /// 読み取り。受け付けられるライフサイクル状態でのみ実行する。
    Read(ReadOperation),
    /// 編集。受け付けられるライフサイクル状態でのみ実行する。
    ///
    /// 一括適用もここに含まれる。受付判定・失敗の写像・実行口が単一の編集と
    /// 同じであり、別の族を立てる理由が無い。
    Edit(EditOperation),
    /// レンダリング。受け付けられるライフサイクル状態でのみ実行する。
    Render(RenderOperation),
}

/// operation 名を処理経路へ対応付ける。
///
/// 名前の一覧は [`KnownOperation`] から導く。族ごとの照合をここへ書き写すと、
/// operation を増やしたときに片方だけへ足し忘れても検出できない。
fn classify_operation(name: &str) -> Result<Operation, ErrorObject> {
    if name == "ping" {
        return Ok(Operation::Ping);
    }
    match KnownOperation::from_operation_name(name) {
        Some(KnownOperation::Read(operation)) => Ok(Operation::Read(operation)),
        Some(KnownOperation::Edit(operation)) => Ok(Operation::Edit(operation)),
        Some(KnownOperation::Render(operation)) => Ok(Operation::Render(operation)),
        None => Err(unsupported_operation()),
    }
}

/// operation ごとの上限と要求の期限から、実行に採用する期限を決める。
///
/// 上限の選択と期限の突き合わせを 1 つの関数にまとめる。要求処理はこれを
/// 呼ぶだけになり、operation ごとの上限が期限の判定へ実際に効いていることを
/// 実時間を待たずに確かめられる。
fn resolve_execution_deadline(
    now: Instant,
    now_unix_ms: i64,
    operation: Operation,
    deadline_unix_ms: Option<u64>,
) -> RequestDeadline {
    resolve_request_deadline(
        now,
        now_unix_ms,
        execution_timeout(operation),
        deadline_unix_ms,
    )
}

/// operation の実行に許す上限を引く。
///
/// 生存確認だけは実行に費やす時間を持たない。状態を読むだけで完結するため、
/// 期限をそのまま応答の送信へ充てる。
///
/// 残りは要求予算の区分から引き、区分の判定そのものは
/// [`KnownOperation::budget_kind`] に委ねる。ここで operation 名や族から直接
/// 引くと、要求元が使う予算の区分と plugin 側の上限が別々の一覧で決まり、
/// 片方だけを変えても気付けない。
///
/// **`match` は `_` を使わない網羅 `match` で書く。** operation の族を足すと
/// 腕が足りずコンパイルが落ちる。上限を決めないまま新しい operation が既定へ
/// 落ちることがない。区分ごとの値は
/// [`ScaledBudgets::plugin_execution`](aviutl2_mcp_core::ScaledBudgets::plugin_execution)
/// が持ち、こちらも網羅で分岐する。
///
/// 編集と一括適用の上限が効くのは編集区間へ入る前の判定に限られる。区間へ
/// 入った後はホストのメインスレッドがコールバックを走らせるまで戻らず、
/// 割り込む手段が無いため、超過しても待つほかない。レンダリングの上限は完了
/// 通知の待ちと成果物の書き出しの合計であり、内訳ごとの上限はレンダリングの
/// 実行口が持つ。
fn execution_timeout(operation: Operation) -> Duration {
    let known = match operation {
        Operation::Ping => return write_timeout(),
        Operation::Read(operation) => KnownOperation::Read(operation),
        Operation::Edit(operation) => KnownOperation::Edit(operation),
        Operation::Render(operation) => KnownOperation::Render(operation),
    };
    budgets().plugin_execution(known.budget_kind())
}

/// 実行口を持たない operation へ返すエラー。
fn unsupported_operation() -> ErrorObject {
    error_object(ErrorCode::UnsupportedOperation, "未対応の operation です")
}

/// params の復号・受付判定・期限判定を通してから読み取りを実行する。
///
/// params の復号を最初に行う。要求内容の誤りはライフサイクル状態にも期限にも
/// 依存せず、状態由来の再試行可能なエラーで返すと要求元に解消しない再試行を
/// 促してしまう。
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
    let request = decode_request(operation, params)?;
    admit_request(state)?;
    if deadline == RequestDeadline::Exceeded {
        // 未開始の要求は中止する。副作用が無いため再試行可能として返す。
        return Err(timeout_before_execution());
    }
    dispatch_read(adapter, request)
}

/// params の復号・受付判定・期限判定を通してから編集を実行する。
///
/// 手順は読み取りと同じで、params の復号を最初に行う。要求内容の誤りは
/// ライフサイクル状態にも期限にも依存せず、状態由来の再試行可能なエラーで返すと
/// 要求元に解消しない再試行を促してしまう。
///
/// 期限を判定できるのは編集区間へ入る前だけである。区間へ入るとホストの
/// メインスレッドがコールバックを走らせるまで戻らず、割り込む手段が無い。
fn execute_edit(
    adapter: &dyn EditAdapter,
    state: &InstanceState,
    operation: EditOperation,
    params: &Value,
    deadline: RequestDeadline,
) -> Result<Value, ErrorObject> {
    let request = decode_edit_request(operation, params)?;
    admit_request(state)?;
    if deadline == RequestDeadline::Exceeded {
        // 未実行のまま中止する。副作用が無いため変更は起きていない。
        return Err(edit_timeout_before_execution());
    }
    dispatch_edit(adapter, request)
}

/// params の復号・受付判定・期限判定を通してからレンダリングを実行する。
///
/// 手順は読み取り・編集と同じで、params の復号を最初に行う。要求内容の誤りは
/// ライフサイクル状態にも期限にも依存せず、状態由来の再試行可能なエラーで返すと
/// 要求元に解消しない再試行を促してしまう。
///
/// 期限を判定できるのはホストへタスクを投入する前だけである。投入したタスクを
/// 取り消す手段は無く、投入後は完了の待ちを打ち切れても、ホストが抱えるタスク
/// そのものは残る。
///
/// 変更の有無を伝えるキーを添えないのは、レンダリングがプロジェクトを一切
/// 変更しないためである。添えると、要求元は編集と同じ警戒を要すると誤解する。
fn execute_render(
    adapter: &dyn RenderAdapter,
    state: &InstanceState,
    operation: RenderOperation,
    params: &Value,
    deadline: RequestDeadline,
) -> Result<RenderFrameResult, ErrorObject> {
    let request = decode_render_request(operation, params)?;
    admit_request(state)?;
    if deadline == RequestDeadline::Exceeded {
        // 未投入のまま中止する。ホストは何も抱えていない。
        return Err(timeout_before_execution());
    }
    dispatch_render(adapter, request)
}

/// 実行前に期限を超過していた要求へ返すエラー。
fn timeout_before_execution() -> ErrorObject {
    error_object(
        ErrorCode::Timeout,
        "要求の deadline を超過したため処理しません",
    )
}

/// 実行前に期限を超過していた編集要求へ返すエラー。
///
/// 変更の有無を機械可読で添える。要求元が観測する `timeout` には、応答を待つ
/// 間に予算が尽きた場合や接続が切れた場合も含まれ、それらでは変更が入ったか
/// どうか分からない。判別できる経路だけが「変更は行われていない」と名乗る。
fn edit_timeout_before_execution() -> ErrorObject {
    error_object(
        ErrorCode::Timeout,
        "要求の deadline を超過したため処理しません",
    )
    .with_details(json!({
        "change_applied": "no",
        "mutation_origin": "plugin",
        "retry_requires": "resend",
    }))
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
        return SendDecision::Send(now + write_timeout());
    };
    if now >= read_deadline {
        return SendDecision::Discard;
    }
    match resolve_request_deadline(now, now_unix_ms, write_timeout(), deadline_unix_ms) {
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
                deadline: now + write_timeout(),
                discarded: true,
            },
            Err(error) => ReadResponse {
                outcome: Err(error),
                deadline: now + write_timeout(),
                discarded: false,
            },
        },
    }
}

/// 結果を破棄しない operation の、応答送信の期限を決める。
///
/// **編集の結果は破棄しない。** 編集が完了していればプロジェクトは変わり、
/// 取り消し単位にも登録されている。結果を破棄すると、適用された変更が要求元
/// からは失敗または無応答として観測される。要求元は自然に再送し、作成は冪等で
/// ないため重複し得る。破棄で節約できるのは高々送信上限の 1 秒であり、適用済み
/// の変更を隠すことと釣り合わない。一括適用は 1 要求で運ぶ変更が多いぶん、
/// この規則の重みが増す。
///
/// **レンダリングの結果も破棄しない。** レンダリングは副作用を持たないため
/// 捨ててもプロジェクトは壊れないが、捨てると引き渡し用ファイルが宙に浮く。
/// 受け取る側は識別子を得ていないため掃除できず、期限切れの掃除まで残る。
/// 送信できれば受け取る側が即座に所有して片付けられる。破棄で節約できるのは
/// 送信上限だけで、レンダリング・符号化・書き出しの費用は既に払い終えている。
///
/// 読み取りが結果を破棄してよいのは「捨てても中途半端な状態も宙に浮くものも
/// 残らない」からであり、編集でもレンダリングでもその前提が成り立たない。
/// **読み取りの破棄経路をこの 2 つへ再利用しない。**
///
/// **送信には常に送信上限をそのまま充てる。要求の期限で縮めない。** 予算配分は
/// 送信の持ち時間を実行とは別枠で確保しており、要求の期限で縮めると、実行が
/// 期限際まで掛かった要求の送信に数ミリ秒しか残らない。送信に失敗すれば適用
/// 済みの変更が要求元からは無応答に見え、結果を破棄しないことで防ごうとした
/// 状況がそのまま起きる。
///
/// **これは読み取りと異なる規則である。** 読み取りは要求の残り時間と送信上限の
/// 短い方を採る。捨ててよい結果と、捨ててはいけない結果の差がここに出る。
fn retained_send_deadline(now: Instant) -> Instant {
    now + write_timeout()
}

/// レンダリングの結果を応答として送り、送れなかった成果物を実行口へ戻す。
///
/// 引き渡し用ファイルの識別子は応答にだけ載る。送信できなければ受け取る側は
/// 識別子を持たず、ファイルを引き取ることも掃除することもできないため、
/// **送る前に識別子を控えておき、送信に失敗したら実行口へ差し戻す。**
/// **送信できた場合は消さない。** 受け取る側が所有し、引き取りを終えたときに
/// 片付ける。失敗した要求は成果物を作っていないため、戻すものを持たない。
///
/// 識別子の控えと差し戻しを 1 つの関数に閉じ込めるのは、控えを取り損ねても
/// 差し戻しの側は動いたままに見えるからである。分けて書くと、控えを落とす
/// 変更が「後始末はあるのに何も消えない」状態として通ってしまう。
///
/// **編集にはこれに対応する後始末が無い。** 変更は既に取り消し単位へ登録されて
/// おり、応答を送れなくても実行口へ差し戻せるものが存在しない。
fn deliver_render_response(
    adapter: &dyn RenderAdapter,
    outcome: Result<RenderFrameResult, ErrorObject>,
    send: impl FnOnce(Result<Value, ErrorObject>) -> Result<()>,
) -> Result<()> {
    // 成果物を持つのは成功した結果だけである。失敗した要求は作っていない。
    let handoff_token = outcome
        .as_ref()
        .ok()
        .map(|result| result.handoff_token.clone());

    // 応答へ載せられたことと、載せた応答を送れたことは別の失敗である。識別子が
    // 要求元へ渡るのは**その両方を満たしたときだけ**であり、どちらか一方でも
    // 欠ければ成果物は宙に浮く。2 つを 1 つの結果へ畳むと、変換に失敗した
    // 応答が「失敗応答の送信に成功した」ものとして後始末を素通りする。
    let body = outcome.and_then(|result| to_result(&result));
    let carries_the_token = body.is_ok();
    let sent = send(body);

    if !(carries_the_token && sent.is_ok())
        && let Some(handoff_token) = handoff_token
    {
        tracing::warn!("応答が要求元へ届かなかったため引き渡し用ファイルを削除します");
        adapter.discard_artifact(&handoff_token);
    }
    sent
}

/// ライフサイクル状態が読み取り・編集を受け付けられるかを判定する。
///
/// 起動処理中は編集ハンドルが読み取り API も編集 API も受け付けられず、呼ぶこと
/// 自体が許されない。終了処理中は新規要求を受け付けない。いずれも要求内容の
/// 誤りではないため、間隔を空けた再試行を案内する。再生・出力中の判別は編集
/// 状態を確かめられる読み取り口・編集口の責務であり、`busy` はここでは通す。
///
/// 判定は読み取りと編集で同一であるため 1 つの関数に置く。説明も両方を指す
/// 文言に揃える。読み取りだけを名指しすると、編集の要求元へ誤った説明が返る。
fn admit_request(state: &InstanceState) -> Result<(), ErrorObject> {
    match state {
        InstanceState::Ready | InstanceState::Busy => Ok(()),
        InstanceState::Starting => Err(host_busy("起動処理中のため要求を受け付けられません")),
        InstanceState::Draining => Err(host_busy("終了処理中のため要求を受け付けられません")),
        // 接続済みの経路では観測されない。この状態へ移る際は descriptor の削除と
        // pipe の切断が先立ち、要求元は接続の失敗として先に検出する。同じ
        // インスタンスが戻ることはないため再試行の間隔を案内せず、インスタンスを
        // 探し直すべき状態として返す。
        InstanceState::Gone => Err(error_object(
            ErrorCode::InstanceStale,
            "インスタンスは既に終了しています",
        )),
        InstanceState::Unknown(_) => Err(host_busy("要求を受け付けられない状態です")),
    }
}

/// 一時的に要求を受け付けられないことを、再試行の案内つきで返す。
fn host_busy(message: &str) -> ErrorObject {
    error_object(ErrorCode::HostBusy, message)
        .with_details(json!({ "retry_after_ms": HOST_BUSY_RETRY_AFTER_MS }))
}

/// 復号と検証を終えた読み取り要求。
///
/// この型を作れた時点で、要求内容だけで判定できる誤りは残っていない。ページ
/// 指定を伴う operation が検証済みのページ要求を併せて運ぶのはそのためである。
/// params が持つ生のページ指定は切り出しへ渡せない。
#[derive(Debug, Clone, PartialEq)]
enum ReadRequest {
    GetEditInfo,
    GetCurrentScene,
    ListLayers(ListLayersParams, ValidatedPageRequest),
    ListObjects(ListObjectsParams, ValidatedPageRequest),
    GetObject(Box<GetObjectParams>),
    ListAvailableEffects(ListAvailableEffectsParams, ValidatedPageRequest),
    GetEffectItemValues(Box<GetEffectItemValuesParams>),
    GetSelection(GetSelectionParams, ValidatedPageRequest),
    ListFonts(ValidatedPageRequest),
    ListPalettes(ValidatedPageRequest),
    ListModules(ListModulesParams, ValidatedPageRequest),
    ListObjectAliases(ListObjectAliasesParams, ValidatedPageRequest),
}

/// operation 別の params を復号し、要求内容だけで決まる検証を済ませる。
///
/// ページ指定と絞り込み条件の検証もここで行う。いずれも要求内容だけで決まり、
/// ライフサイクル状態にも期限にも読み取り口の応答にも依存しない。
fn decode_request(operation: ReadOperation, params: &Value) -> Result<ReadRequest, ErrorObject> {
    Ok(match operation {
        ReadOperation::GetEditInfo => {
            decode_params::<GetEditInfoParams>(params)?;
            ReadRequest::GetEditInfo
        }
        ReadOperation::GetCurrentScene => {
            decode_params::<GetCurrentSceneParams>(params)?;
            ReadRequest::GetCurrentScene
        }
        ReadOperation::ListLayers => {
            let params: ListLayersParams = decode_params(params)?;
            let page = params.page.validate().map_err(page_limit_error)?;
            ReadRequest::ListLayers(params, page)
        }
        ReadOperation::ListObjects => {
            let params: ListObjectsParams = decode_params(params)?;
            let page = params.page.validate().map_err(page_limit_error)?;
            if let Some(filter) = &params.filter {
                filter.validate().map_err(filter_error)?;
            }
            ReadRequest::ListObjects(params, page)
        }
        ReadOperation::GetObject => {
            ReadRequest::GetObject(Box::new(decode_params::<GetObjectParams>(params)?))
        }
        ReadOperation::ListAvailableEffects => {
            let params: ListAvailableEffectsParams = decode_params(params)?;
            let page = params.page.validate().map_err(page_limit_error)?;
            ReadRequest::ListAvailableEffects(params, page)
        }
        ReadOperation::GetEffectItemValues => {
            let params: GetEffectItemValuesParams = decode_params(params)?;
            params.validate().map_err(item_values_error)?;
            ReadRequest::GetEffectItemValues(Box::new(params))
        }
        ReadOperation::GetSelection => {
            let params: GetSelectionParams = decode_params(params)?;
            let page = params.page.validate().map_err(page_limit_error)?;
            ReadRequest::GetSelection(params, page)
        }
        ReadOperation::ListFonts => {
            let params: ListFontsParams = decode_params(params)?;
            ReadRequest::ListFonts(params.page.validate().map_err(page_limit_error)?)
        }
        ReadOperation::ListPalettes => {
            let params: ListPalettesParams = decode_params(params)?;
            ReadRequest::ListPalettes(params.page.validate().map_err(page_limit_error)?)
        }
        ReadOperation::ListModules => {
            let params: ListModulesParams = decode_params(params)?;
            let page = params.page.validate().map_err(page_limit_error)?;
            ReadRequest::ListModules(params, page)
        }
        ReadOperation::ListObjectAliases => {
            let params: ListObjectAliasesParams = decode_params(params)?;
            let page = params.page.validate().map_err(page_limit_error)?;
            params.validate().map_err(label_error)?;
            ReadRequest::ListObjectAliases(params, page)
        }
    })
}

/// 読み取りを実行し、応答へ載せる result を組み立てる。
///
/// 読み取り口は SDK の参照区間を抜けてから所有型の DTO を返す。JSON への変換は
/// その外側で行い、参照区間の内側には持ち込まない。
///
/// ページの切り出しも原則ここで行うが、オブジェクトの列挙だけは読み取り口が
/// 参照区間の内側で切り出す。1 件の読み取りが重く、応答へ載せない対象まで読むと
/// 参照区間の保持時間がプロジェクトの規模で決まってしまうためである。
fn dispatch_read(adapter: &dyn ReadAdapter, request: ReadRequest) -> Result<Value, ErrorObject> {
    match request {
        ReadRequest::GetEditInfo => to_result(&adapter.get_edit_info().map_err(read_error)?),
        ReadRequest::GetCurrentScene => {
            let (scene, project_revision) = adapter.get_current_scene().map_err(read_error)?;
            to_result(&GetCurrentSceneResult {
                scene,
                project_revision,
            })
        }
        ReadRequest::ListLayers(params, request) => {
            let snapshot = adapter
                .list_layers(params.expected_scene_id)
                .map_err(read_error)?;
            let (items, page) = take_page(&snapshot.items, &request, snapshot.snapshot_revision)
                .map_err(snapshot_revision_error)?;
            to_result(&ListLayersResult { items, page })
        }
        ReadRequest::ListObjects(params, request) => {
            // 切り出しは読み取り口が済ませている。参照区間の失敗と、ページ要求
            // そのものの不整合は別の失敗であり、対応するエラーも異なる。
            let page = adapter
                .list_objects(params.expected_scene_id, params.filter.as_ref(), &request)
                .map_err(read_error)?
                .map_err(snapshot_revision_error)?;
            to_result(&ListObjectsResult {
                items: page.items,
                page: page.meta,
            })
        }
        ReadRequest::GetObject(params) => {
            to_result(&adapter.get_object(&params.selector).map_err(read_error)?)
        }
        ReadRequest::ListAvailableEffects(params, request) => {
            let snapshot = adapter
                .list_available_effects(params.effect_type.as_ref())
                .map_err(read_error)?;
            let (items, page) = take_window(
                &snapshot.items,
                &catalog_page_request(&request),
                snapshot.snapshot_revision,
            );
            to_result(&ListAvailableEffectsResult { items, page })
        }
        ReadRequest::GetEffectItemValues(params) => to_result(
            &adapter
                .get_effect_item_values(&params)
                .map_err(read_error)?,
        ),
        ReadRequest::GetSelection(params, request) => {
            // 切り出しは読み取り口が済ませている。参照区間の失敗と、ページ要求
            // そのものの不整合は別の失敗であり、対応するエラーも異なる。
            let snapshot: SelectionSnapshot = adapter
                .get_selection(params.expected_scene_id, &request)
                .map_err(read_error)?
                .map_err(snapshot_revision_error)?;
            to_result(&snapshot)
        }
        ReadRequest::ListFonts(request) => {
            let snapshot = adapter.list_fonts().map_err(read_error)?;
            let (items, page) = take_window(
                &snapshot.items,
                &catalog_page_request(&request),
                snapshot.snapshot_revision,
            );
            to_result(&ListFontsResult { items, page })
        }
        ReadRequest::ListPalettes(request) => {
            // 切り出しは読み取り口が済ませている。色を読むのが窓に入った分だけで
            // あることを、参照区間の内側で保証する必要がある。
            let result = adapter
                .list_palettes(&catalog_page_request(&request))
                .map_err(read_error)?;
            to_result(&result)
        }
        ReadRequest::ListModules(params, request) => {
            let snapshot = adapter
                .list_modules(params.module_type.as_ref())
                .map_err(read_error)?;
            let (items, page) = take_window(
                &snapshot.items,
                &catalog_page_request(&request),
                snapshot.snapshot_revision,
            );
            to_result(&ListModulesResult { items, page })
        }
        ReadRequest::ListObjectAliases(params, request) => {
            // 切り出しは読み取り口が済ませている。ファイルを開くのが窓に入った
            // 分だけであることを、切り出しと同じ場所で保証する必要がある。
            let result = adapter
                .list_object_aliases(params.label.as_deref(), &catalog_page_request(&request))
                .map_err(read_error)?;
            to_result(&result)
        }
    }
}

/// カタログの一覧に対するページ要求から revision の照合指定を落とす。
///
/// 登録済み effect・フォント・パレット・モジュールはいずれも、プロジェクトの
/// 編集内容から独立した登録物の集合である。要求元が前ページの revision を送り
/// 返しても照合しない。照合すると、一覧と無関係な編集で値が進んだだけでページ間の
/// 照合が食い違い、要求元は先頭からの取り直しを強いられる。一方でカタログ自身の
/// 変化はその値に現れないため、照合しても取りこぼしは防げない。
///
/// 応答へ載せる revision は落とさない。それは列挙を始めた時点のプロジェクト
/// revision であり、ページのメタ情報が表す意味そのものである。照合に使えない
/// ことを表す固定値へ置き換えても、実在し得る revision と区別が付かない。
///
/// 落とした結果は取り出し範囲であり、切り出しは失敗しない。照合しないと決めた
/// ことが、失敗の種類が 0 であることとして型に現れる。
fn catalog_page_request(page: &ValidatedPageRequest) -> PageWindow {
    page.window()
}

/// 復号と検証を終えた編集要求。
///
/// この型を作れた時点で、要求内容だけで判定できる誤りは残っていない。
#[derive(Debug, Clone, PartialEq)]
enum EditRequest {
    CreateObject(Box<CreateObjectParams>),
    MoveObject(Box<MoveObjectParams>),
    DeleteObject(Box<DeleteObjectParams>),
    SetObjectName(Box<SetObjectNameParams>),
    SetObjectItem(Box<SetObjectItemParams>),
    AddEffect(Box<AddEffectParams>),
    DeleteEffect(Box<DeleteEffectParams>),
    SetEffectEnabled(Box<SetEffectEnabledParams>),
    SetLayerState(Box<SetLayerStateParams>),
    SetSelection(Box<SetSelectionParams>),
    CreateObjectSection(Box<CreateObjectSectionParams>),
    DeleteObjectSection(Box<DeleteObjectSectionParams>),
    MoveObjectSection(Box<MoveObjectSectionParams>),
    SetGridBpm(Box<SetGridBpmParams>),
    SetSceneSettings(Box<SetSceneSettingsParams>),
    ApplyBatch(Box<ApplyBatchParams>),
}

/// operation 別の params を復号し、要求内容だけで決まる検証を済ませる。
///
/// 値の種別整合・パス構文・文字列長・変更内容の全省略はいずれも要求内容だけで
/// 決まり、ライフサイクル状態にも期限にも編集口の応答にも依存しない。検証の
/// 実体は core と共有し、server と plugin が同じ判定を行う。
///
/// 一括適用は各 sub-operation について単独編集と同じ検証を通し、加えて件数・
/// シーンの揃い・同じ状態を書き換える重複を見る。いずれも要求内容だけで決まる
/// ため、他の編集と同じくこの段で判定する。
fn decode_edit_request(
    operation: EditOperation,
    params: &Value,
) -> Result<EditRequest, ErrorObject> {
    /// 復号と検証を済ませて要求を組み立てる。
    macro_rules! decoded {
        ($ty:ty, $variant:path) => {{
            let params: $ty = decode_params(params)?;
            params.validate().map_err(edit_input_error)?;
            $variant(Box::new(params))
        }};
    }
    Ok(match operation {
        EditOperation::CreateObject => {
            decoded!(CreateObjectParams, EditRequest::CreateObject)
        }
        EditOperation::MoveObject => decoded!(MoveObjectParams, EditRequest::MoveObject),
        EditOperation::DeleteObject => decoded!(DeleteObjectParams, EditRequest::DeleteObject),
        EditOperation::SetObjectName => {
            decoded!(SetObjectNameParams, EditRequest::SetObjectName)
        }
        EditOperation::SetObjectItem => {
            decoded!(SetObjectItemParams, EditRequest::SetObjectItem)
        }
        EditOperation::AddEffect => decoded!(AddEffectParams, EditRequest::AddEffect),
        EditOperation::DeleteEffect => decoded!(DeleteEffectParams, EditRequest::DeleteEffect),
        EditOperation::SetEffectEnabled => {
            decoded!(SetEffectEnabledParams, EditRequest::SetEffectEnabled)
        }
        EditOperation::SetLayerState => {
            decoded!(SetLayerStateParams, EditRequest::SetLayerState)
        }
        EditOperation::SetSelection => decoded!(SetSelectionParams, EditRequest::SetSelection),
        EditOperation::CreateObjectSection => {
            decoded!(CreateObjectSectionParams, EditRequest::CreateObjectSection)
        }
        EditOperation::DeleteObjectSection => {
            decoded!(DeleteObjectSectionParams, EditRequest::DeleteObjectSection)
        }
        EditOperation::MoveObjectSection => {
            decoded!(MoveObjectSectionParams, EditRequest::MoveObjectSection)
        }
        EditOperation::SetGridBpm => decoded!(SetGridBpmParams, EditRequest::SetGridBpm),
        EditOperation::SetSceneSettings => {
            decoded!(SetSceneSettingsParams, EditRequest::SetSceneSettings)
        }
        EditOperation::ApplyBatch => {
            let params: ApplyBatchParams = decode_params(params)?;
            params.validate().map_err(batch_input_error)?;
            EditRequest::ApplyBatch(Box::new(params))
        }
    })
}

/// 復号と検証を終えたレンダリング要求。
///
/// この型を作れた時点で、要求内容だけで判定できる誤りは残っていない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderRequest {
    RenderFrame(RenderFrameParams),
}

/// operation 別の params を復号し、要求内容だけで決まる検証を済ませる。
///
/// ここで見るのはフレーム番号が受け渡せる範囲に収まることだけである。シーンの
/// 長さとの比較は編集情報を要するため、実行口が判定する。
fn decode_render_request(
    operation: RenderOperation,
    params: &Value,
) -> Result<RenderRequest, ErrorObject> {
    Ok(match operation {
        RenderOperation::RenderFrame => {
            let params: RenderFrameParams = decode_params(params)?;
            params.validate().map_err(render_input_error)?;
            RenderRequest::RenderFrame(params)
        }
    })
}

/// 編集を実行し、応答へ載せる result を組み立てる。
///
/// 編集口は SDK の編集区間を抜けてから所有型の DTO を返す。JSON への変換は
/// その外側で行い、区間の内側には持ち込まない。
fn dispatch_edit(adapter: &dyn EditAdapter, request: EditRequest) -> Result<Value, ErrorObject> {
    match request {
        EditRequest::CreateObject(params) => {
            to_result(&adapter.create_object(&params).map_err(edit_error)?)
        }
        EditRequest::MoveObject(params) => {
            to_result(&adapter.move_object(&params).map_err(edit_error)?)
        }
        EditRequest::DeleteObject(params) => {
            to_result(&adapter.delete_object(&params).map_err(edit_error)?)
        }
        EditRequest::SetObjectName(params) => {
            to_result(&adapter.set_object_name(&params).map_err(edit_error)?)
        }
        EditRequest::SetObjectItem(params) => {
            to_result(&adapter.set_object_item(&params).map_err(edit_error)?)
        }
        EditRequest::AddEffect(params) => {
            to_result(&adapter.add_effect(&params).map_err(edit_error)?)
        }
        EditRequest::DeleteEffect(params) => {
            to_result(&adapter.delete_effect(&params).map_err(edit_error)?)
        }
        EditRequest::SetEffectEnabled(params) => {
            to_result(&adapter.set_effect_enabled(&params).map_err(edit_error)?)
        }
        EditRequest::SetLayerState(params) => {
            to_result(&adapter.set_layer_state(&params).map_err(edit_error)?)
        }
        EditRequest::SetSelection(params) => {
            to_result(&adapter.set_selection(&params).map_err(edit_error)?)
        }
        EditRequest::CreateObjectSection(params) => {
            to_result(&adapter.create_object_section(&params).map_err(edit_error)?)
        }
        EditRequest::DeleteObjectSection(params) => {
            to_result(&adapter.delete_object_section(&params).map_err(edit_error)?)
        }
        EditRequest::MoveObjectSection(params) => {
            to_result(&adapter.move_object_section(&params).map_err(edit_error)?)
        }
        EditRequest::SetGridBpm(params) => {
            to_result(&adapter.set_grid_bpm(&params).map_err(edit_error)?)
        }
        EditRequest::SetSceneSettings(params) => {
            to_result(&adapter.set_scene_settings(&params).map_err(edit_error)?)
        }
        EditRequest::ApplyBatch(params) => {
            to_result(&adapter.apply_batch(&params).map_err(edit_error)?)
        }
    }
}

/// レンダリングを実行し、応答へ載せる result を組み立てる。
///
/// JSON への変換はここでは行わない。応答を送れなかったときに引き渡し用ファイル
/// を消せるよう、識別子を持つ所有型のまま呼び出し元へ返す。
fn dispatch_render(
    adapter: &dyn RenderAdapter,
    request: RenderRequest,
) -> Result<RenderFrameResult, ErrorObject> {
    match request {
        RenderRequest::RenderFrame(params) => adapter.render_frame(&params).map_err(render_error),
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

/// 編集の失敗を応答用のエラーへ変換する。
fn edit_error(error: EditError) -> ErrorObject {
    ErrorObject::new(error.error_code(), error.to_string(), error.retryable())
        .with_details(error.details())
}

/// レンダリングの失敗を応答用のエラーへ変換する。
fn render_error(error: RenderError) -> ErrorObject {
    ErrorObject::new(error.error_code(), error.to_string(), error.retryable())
        .with_details(error.details())
}

/// 入力検証の失敗へ添える補助情報を組み立てる。値が 1 つも無ければ `None`。
///
/// 添えるのは失敗の種別名と、一括適用で落ちた sub-operation の位置だけである。
/// どちらも要求元の内容を反響させず、設定値・alias・パスそのものを含まない。
fn input_error_details(reason: Option<&str>, failed_index: Option<usize>) -> Option<Value> {
    let mut details = Map::new();
    if let Some(reason) = reason {
        details.insert("reason".to_string(), json!(reason));
    }
    if let Some(index) = failed_index {
        details.insert("failed_index".to_string(), json!(index));
    }
    (!details.is_empty()).then_some(Value::Object(details))
}

/// 組み立てた補助情報があればエラーへ添える。
fn with_details(mapped: ErrorObject, details: Option<Value>) -> ErrorObject {
    match details {
        Some(details) => mapped.with_details(details),
        None => mapped,
    }
}

/// 要求内容だけで決まる検証の失敗を応答用のエラーへ変換する。
///
/// **どの規則で落ちたかを機械可読な形で添える。** パスの構文検証は 7 種、
/// 文字列の構文検証は 4 種の失敗を持ち、要求元が取れる行動はそれぞれ異なる
/// （ローカルへ複製する・絶対パスにする・短い場所へ移す）。名前が無ければ、
/// 要求元は説明の文面を解析するほかない。
///
/// 失敗の説明にも補助情報にも、対象フィールド名と規則の上限だけが現れる。
/// 設定値・alias・パスそのものは含まない。
pub(crate) fn edit_input_error(error: EditInputError) -> ErrorObject {
    with_details(
        error_object(error.error_code(), error.to_string()),
        input_error_details(error.reason(), None),
    )
}

/// 一括適用の要求内容だけで決まる検証の失敗を応答用のエラーへ変換する。
///
/// **何番目の sub-operation で落ちたかを機械可読な形で添える。** 100 件までを
/// 1 要求で運ぶ operation に対し、位置の分からない `invalid_argument` は訂正の
/// 手掛かりとして足りない。位置は説明の文面にも現れるが、要求元に文面の解析を
/// 強いない。要求全体の誤り（件数）は位置を持たないため添えない。
///
/// **要求元がこの層へ届く前に同じ検証を通っているとは限らない。** 検証を備えた
/// 口を経由しない要求でも、位置は同じ形で返る必要がある。
///
/// **sub-operation の失敗は単独編集と同じ名前を添える。** 同じ入力が経路に
/// よって違う応答になれば、要求元は一括適用のためだけの分岐を持つことになる。
///
/// 添えるのは 0 始まりの整数と失敗の種別名だけである。失敗の説明に現れるのは
/// 対象フィールド名と規則の上限に限られ、設定値・alias・パスそのものは含まない。
pub(crate) fn batch_input_error(error: BatchInputError) -> ErrorObject {
    with_details(
        error_object(error.error_code(), error.to_string()),
        input_error_details(error.reason(), error.failed_index()),
    )
}

/// レンダリングの要求内容だけで決まる検証の失敗を応答用のエラーへ変換する。
///
/// この検証が見るのはフレーム番号が受け渡せる範囲に収まることだけであり、
/// 失敗の種別は 1 つしかない。分岐する先が無いため名前は添えない。
fn render_input_error(error: RenderInputError) -> ErrorObject {
    error_object(error.error_code(), error.to_string())
}

/// ページ指定の範囲の失敗を応答用のエラーへ変換する。
fn page_limit_error(error: LimitOutOfRange) -> ErrorObject {
    error_object(ErrorCode::InvalidArgument, error.to_string())
}

/// ページ間の revision 照合の失敗を応答用のエラーへ変換する。
fn snapshot_revision_error(error: SnapshotRevisionMismatch) -> ErrorObject {
    error_object(
        ErrorCode::PreconditionFailed,
        "一覧が変化したため、先頭のページから取り直してください",
    )
    .with_details(json!({
        "requested_snapshot_revision": error.requested,
        "current_snapshot_revision": error.current,
    }))
}

/// 絞り込み条件の失敗を応答用のエラーへ変換する。
fn filter_error(error: ObjectFilterError) -> ErrorObject {
    error_object(ErrorCode::InvalidArgument, error.to_string())
}

/// 文字列の構文の失敗を応答用のエラーへ変換する。
///
/// 添えるのは失敗の種別名だけである。検証対象の文字列そのものは、説明の文面
/// にも補助情報にも現れない。
fn label_error(error: TextSyntaxError) -> ErrorObject {
    with_details(
        error_object(ErrorCode::InvalidArgument, error.to_string()),
        input_error_details(Some(error.reason()), None),
    )
}

/// 補間後の値の要求内容の失敗を応答用のエラーへ変換する。
///
/// 見るのは件数と重複と項目名の規則だけであり、どれも説明の文面で訂正できる。
/// 分岐に使う名前は添えない。
fn item_values_error(error: EffectItemValuesInputError) -> ErrorObject {
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
    request_id: RequestId,
    instance_id: InstanceId,
    outcome: Result<serde_json::Value, ErrorObject>,
) -> ResponseEnvelope {
    ResponseEnvelope {
        kind: ResponseKind::Response,
        protocol_version: ProtocolVersion::CURRENT,
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
    request_id: RequestId,
    instance_id: InstanceId,
    error: ErrorObject,
) -> Result<()> {
    let response = response_envelope(request_id, instance_id, Err(error));
    send_response(stream, &response, Instant::now() + write_timeout())
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
    use crate::read::{Page, Snapshot};
    use aviutl2_mcp_core::{
        AvailableEffect, AvailableEffectItem, Cursor, DisplayRange, EditInfo, EffectFlags,
        EffectItemType, EffectItemValues, EffectSelector, EffectType, EvaluatedItem, Extent,
        FiniteF64, FrameRange, LayerInfo, ListObjectAliasesResult, ListPalettesResult,
        MAX_EVALUATED_FRAMES, MAX_EVALUATED_ITEMS, ModuleEntry, ModuleType, ObjectAliasSummary,
        ObjectDetail, ObjectFilter, ObjectFingerprintInput, ObjectSelector, ObjectSummary,
        PALETTE_COLOR_COUNT, PaletteEntry, RequestBudgetKind, Rgba, SceneInfo, SectionRange,
    };
    use std::sync::Mutex;

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
        fn project_status(&self) -> crate::read::ProjectStatus {
            self.calls.lock().unwrap().push("project_status");
            crate::read::ProjectStatus {
                epoch: EPOCH.to_string(),
                revision: REVISION,
                modified: true,
            }
        }

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
            page: &ValidatedPageRequest,
        ) -> Result<Result<Page<ObjectSummary>, SnapshotRevisionMismatch>, ReadError> {
            self.enter("list_objects")?;
            ensure_scene(expected_scene_id)?;
            let layer_min = filter.and_then(|filter| filter.layer_min).unwrap_or(0);
            let items: Vec<ObjectSummary> = fake_objects()
                .into_iter()
                .filter(|object| object.layer >= layer_min)
                .collect();
            Ok(take_page(&items, page, REVISION).map(|(items, meta)| Page { items, meta }))
        }

        fn get_object(&self, selector: &ObjectSelector) -> Result<ObjectDetail, ReadError> {
            self.enter("get_object")?;
            let summary = fake_object();
            if *selector != summary.selector {
                return Err(ReadError::ObjectNotFound {
                    detected_by: "find_object",
                });
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

        fn list_fonts(&self) -> Result<Snapshot<String>, ReadError> {
            self.enter("list_fonts")?;
            Ok(Snapshot {
                items: fake_fonts(),
                snapshot_revision: REVISION,
            })
        }

        fn list_palettes(&self, page: &PageWindow) -> Result<ListPalettesResult, ReadError> {
            self.enter("list_palettes")?;
            let names = fake_palette_names();
            let (window, meta) = take_window(&names, page, REVISION);
            Ok(ListPalettesResult {
                current: Some("[標準.既定]".to_string()),
                items: window
                    .into_iter()
                    .map(|name| PaletteEntry {
                        name,
                        colors: vec![
                            Rgba {
                                r: 0,
                                g: 0,
                                b: 0,
                                a: 255
                            };
                            PALETTE_COLOR_COUNT
                        ],
                    })
                    .collect(),
                page: meta,
            })
        }

        fn list_modules(
            &self,
            module_type: Option<&ModuleType>,
        ) -> Result<Snapshot<ModuleEntry>, ReadError> {
            self.enter("list_modules")?;
            let mut items = fake_modules();
            if let Some(module_type) = module_type {
                items.retain(|module| module.module_type == *module_type);
            }
            Ok(Snapshot {
                items,
                snapshot_revision: REVISION,
            })
        }

        fn list_object_aliases(
            &self,
            label: Option<&str>,
            page: &PageWindow,
        ) -> Result<ListObjectAliasesResult, ReadError> {
            self.enter("list_object_aliases")?;
            let mut items = fake_object_aliases();
            if let Some(label) = label {
                items.retain(|item| item.label.as_deref() == Some(label));
            }
            // 呼ぶたびに revision が進む。照合を外したことが実際に効いているか
            // は、進んだ後の 2 ページ目が通るかでしか見えない。
            let calls = self
                .calls()
                .iter()
                .filter(|call| **call == "list_object_aliases")
                .count() as u64;
            let revision = REVISION + calls - 1;
            let (items, page) = take_window(&items, page, revision);
            Ok(ListObjectAliasesResult { items, page })
        }

        fn get_effect_item_values(
            &self,
            params: &GetEffectItemValuesParams,
        ) -> Result<EffectItemValues, ReadError> {
            self.enter("get_effect_item_values")?;
            Ok(EffectItemValues {
                project_revision: REVISION,
                frames: params.frames.clone(),
                items: vec![EvaluatedItem::Track {
                    name: "X".to_string(),
                    values: params.frames.clone(),
                    group: None,
                }],
                truncated: false,
            })
        }

        fn get_selection(
            &self,
            expected_scene_id: i32,
            page: &ValidatedPageRequest,
        ) -> Result<Result<SelectionSnapshot, SnapshotRevisionMismatch>, ReadError> {
            self.enter("get_selection")?;
            ensure_scene(expected_scene_id)?;
            let items = fake_objects();
            Ok(
                take_page(&items, page, REVISION).map(|(selected, meta)| SelectionSnapshot {
                    project_revision: REVISION,
                    focus: Some(fake_object()),
                    focus_section: Some(1),
                    selected,
                    page: meta,
                }),
            )
        }
    }

    /// レイヤー 1・フレーム 100 のオブジェクトが持つ effect を指すセレクター。
    fn fake_effect_selector() -> EffectSelector {
        let object = fake_object().selector;
        EffectSelector {
            fingerprint: object.fingerprint.clone(),
            object,
            effect_name: "動画ファイル".to_string(),
            effect_index: 0,
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

    fn fake_fonts() -> Vec<String> {
        vec![
            "MS UI Gothic".to_string(),
            "游ゴシック".to_string(),
            "Segoe UI".to_string(),
        ]
    }

    fn fake_palette_names() -> Vec<String> {
        vec!["既定".to_string(), "暖色".to_string(), "寒色".to_string()]
    }

    fn fake_object_aliases() -> Vec<ObjectAliasSummary> {
        vec![
            ObjectAliasSummary {
                name: "テロップ".to_string(),
                label: Some("テロップ集".to_string()),
                object_count: Some(1),
                effects: vec!["テキスト".to_string(), "標準描画".to_string()],
            },
            ObjectAliasSummary {
                name: "背景".to_string(),
                label: None,
                object_count: Some(2),
                effects: vec!["図形".to_string()],
            },
        ]
    }

    fn fake_modules() -> Vec<ModuleEntry> {
        vec![
            ModuleEntry {
                module_type: ModuleType::ScriptObject,
                name: "テキスト".to_string(),
                information: "標準搭載".to_string(),
            },
            ModuleEntry {
                module_type: ModuleType::PluginInput,
                name: "入力プラグイン".to_string(),
                information: "動画の読み込み".to_string(),
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
            RequestDeadline::Within(Instant::now() + read_timeout()),
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
            (
                ReadOperation::GetEffectItemValues,
                json!({ "effect": fake_effect_selector(), "frames": [100.0] }),
            ),
            (
                ReadOperation::GetSelection,
                json!({ "expected_scene_id": SCENE_ID }),
            ),
            (ReadOperation::ListFonts, json!({})),
            (ReadOperation::ListPalettes, json!({})),
            (ReadOperation::ListModules, json!({})),
            (ReadOperation::ListObjectAliases, json!({})),
        ]
    }

    /// [`all_operations`] が全 read operation を含むことを固定する。
    ///
    /// 表は手書きであり、載せ忘れた operation は表を使う検査を全て素通りする。
    /// 応答の秘匿・期限超過・状態の検査はいずれもこの表を材料にしている。
    #[test]
    fn all_operations_covers_every_read_operation() {
        let covered: std::collections::BTreeSet<&str> = all_operations()
            .iter()
            .map(|(operation, _)| operation.as_str())
            .collect();
        let expected: std::collections::BTreeSet<&str> = ReadOperation::ALL
            .iter()
            .map(|operation| operation.as_str())
            .collect();
        assert_eq!(covered, expected);
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
            ("get_effect_item_values", ReadOperation::GetEffectItemValues),
            ("get_selection", ReadOperation::GetSelection),
            ("list_fonts", ReadOperation::ListFonts),
            ("list_palettes", ReadOperation::ListPalettes),
            ("list_modules", ReadOperation::ListModules),
            ("list_object_aliases", ReadOperation::ListObjectAliases),
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
        for name in ["", "Ping", "future_operation", "list_layer"] {
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
    fn effect_item_values_bound_the_frame_and_item_counts_before_reading() {
        // 件数は要求内容だけで決まる。読み取りへ進む前に落とす。
        let selector = fake_effect_selector();
        let over_frames: Vec<f64> = (0..=MAX_EVALUATED_FRAMES)
            .map(|index| index as f64)
            .collect();
        let over_items: Vec<String> = (0..=MAX_EVALUATED_ITEMS)
            .map(|index| format!("項目{index}"))
            .collect();
        for params in [
            json!({ "effect": selector, "frames": [] }),
            json!({ "effect": selector, "frames": over_frames }),
            json!({ "effect": selector, "frames": [100.0], "items": [] }),
            json!({ "effect": selector, "frames": [100.0], "items": over_items }),
        ] {
            let adapter = FakeAdapter::new();
            let error =
                read(&adapter, ReadOperation::GetEffectItemValues, params.clone()).unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::InvalidArgument,
                "{params} が受理されました"
            );
            assert!(
                adapter.calls().is_empty(),
                "{params} が読み取りへ進みました"
            );
        }
    }

    #[test]
    fn effect_item_values_accept_the_counts_at_the_bounds() {
        let selector = fake_effect_selector();
        let frames: Vec<f64> = (0..MAX_EVALUATED_FRAMES)
            .map(|index| index as f64)
            .collect();
        let items: Vec<String> = (0..MAX_EVALUATED_ITEMS)
            .map(|index| format!("項目{index}"))
            .collect();
        let adapter = FakeAdapter::new();
        let result = read(
            &adapter,
            ReadOperation::GetEffectItemValues,
            json!({ "effect": selector, "frames": frames, "items": items }),
        )
        .expect("上限ちょうどが拒否されました");
        assert_eq!(
            result["frames"].as_array().unwrap().len(),
            MAX_EVALUATED_FRAMES
        );
        assert_eq!(adapter.calls(), vec!["get_effect_item_values"]);
    }

    #[test]
    fn effect_item_values_reject_duplicates_before_reading() {
        // 重複も要求内容だけで決まる。同じ値を 2 度評価させず、応答の件数が
        // 要求の件数と対応したままになる。
        let selector = fake_effect_selector();
        for params in [
            json!({ "effect": selector, "frames": [100.0, 100.0] }),
            json!({ "effect": selector, "frames": [100.0], "items": ["範囲", "範囲"] }),
        ] {
            let adapter = FakeAdapter::new();
            let error =
                read(&adapter, ReadOperation::GetEffectItemValues, params.clone()).unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::InvalidArgument,
                "{params} が受理されました"
            );
            assert!(
                adapter.calls().is_empty(),
                "{params} が読み取りへ進みました"
            );
        }
    }

    #[test]
    fn the_effect_item_values_payload_carries_no_handle_or_alias() {
        // 値そのものは載せるが、対象を指す内部の値と alias は載せない。
        let adapter = FakeAdapter::new();
        let result = read(
            &adapter,
            ReadOperation::GetEffectItemValues,
            json!({ "effect": fake_effect_selector(), "frames": [100.0, 100.5] }),
        )
        .expect("評価できます");
        let payload = result.to_string();
        for forbidden in ["alias", "handle", "selector", "0x"] {
            assert!(
                !payload.contains(forbidden),
                "{forbidden} が IPC 応答に現れました: {payload}"
            );
        }
    }

    #[test]
    fn the_catalog_payloads_carry_no_handle_or_alias() {
        // 登録物の名前と属性しか載せない。対象を指す内部の値は現れない。
        for operation in CATALOG_OPERATIONS {
            let adapter = FakeAdapter::new();
            let payload = read(&adapter, operation, json!({}))
                .unwrap_or_else(|error| panic!("{operation:?}: {error:?}"))
                .to_string();
            for forbidden in ["alias", "handle", "selector", "0x"] {
                assert!(
                    !payload.contains(forbidden),
                    "{operation:?} の IPC 応答へ {forbidden} が現れました: {payload}"
                );
            }
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
            (
                ReadOperation::GetSelection,
                json!({ "expected_scene_id": SCENE_ID }),
            ),
            (ReadOperation::ListFonts, json!({})),
            (ReadOperation::ListPalettes, json!({})),
            (ReadOperation::ListModules, json!({})),
            (ReadOperation::ListObjectAliases, json!({})),
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
        // 現在の revision から離れた値を送る。要求元が前ページの値を送り返す
        // 経路と、まったく身に覚えの無い値を送る経路の双方が同じ失敗になる。
        const STALE: u64 = 999;

        let paged = [
            (
                ReadOperation::ListLayers,
                json!({ "expected_scene_id": SCENE_ID, "snapshot_revision": STALE }),
            ),
            (
                ReadOperation::ListObjects,
                json!({ "expected_scene_id": SCENE_ID, "snapshot_revision": STALE }),
            ),
            // 選択はプロジェクトの状態であり revision に連動する。カタログの
            // 一覧と違い、照合の対象になる。
            (
                ReadOperation::GetSelection,
                json!({ "expected_scene_id": SCENE_ID, "snapshot_revision": STALE }),
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
            // 文言は要求元が次に何をすればよいかを述べる唯一の口である。
            assert_eq!(
                error.message, "一覧が変化したため、先頭のページから取り直してください",
                "{operation:?}"
            );
            assert!(error.retryable);
            assert_eq!(error.details["requested_snapshot_revision"], STALE);
            assert_eq!(error.details["current_snapshot_revision"], REVISION);
        }
    }

    #[test]
    fn get_selection_returns_the_focus_its_section_and_the_selection() {
        let adapter = FakeAdapter::new();
        let result = read(
            &adapter,
            ReadOperation::GetSelection,
            json!({ "expected_scene_id": SCENE_ID }),
        )
        .unwrap();

        assert_eq!(result["project_revision"], REVISION);
        assert_eq!(result["focus"]["layer"], 1);
        assert_eq!(result["focus_section"], 1);
        assert_eq!(result["selected"].as_array().unwrap().len(), 2);
        assert_eq!(result["page"]["total_count"], 2);
        assert_eq!(adapter.calls(), vec!["get_selection"]);
    }

    #[test]
    fn get_selection_carries_neither_the_cursor_nor_the_selected_range() {
        // どちらも get_edit_info が既に返している。同じ値を 2 つの読み取りが
        // 返すと、要求元は「どちらが新しいか」を判断する規則を持つことになる。
        let adapter = FakeAdapter::new();
        let result = read(
            &adapter,
            ReadOperation::GetSelection,
            json!({ "expected_scene_id": SCENE_ID }),
        )
        .unwrap();

        let fields = result.as_object().expect("オブジェクト");
        for forbidden in ["cursor", "selected_range", "display"] {
            assert!(
                !fields.contains_key(forbidden),
                "{forbidden} が応答に現れました: {result}"
            );
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

    /// ページ間の revision 照合を行わない列挙。
    ///
    /// いずれも登録物の集合であり、プロジェクトの編集内容から独立している。
    const CATALOG_OPERATIONS: [ReadOperation; 5] = [
        ReadOperation::ListAvailableEffects,
        ReadOperation::ListFonts,
        ReadOperation::ListPalettes,
        ReadOperation::ListModules,
        ReadOperation::ListObjectAliases,
    ];

    #[test]
    fn catalog_pages_ignore_snapshot_revision() {
        // 無関係な編集で revision が進んでも、2 ページ目以降は拒否されない。
        // 照合すると、一覧と関わりの無い編集で先頭からの取り直しを強いる一方、
        // 一覧自身の変化はその値に現れないため取りこぼしも防げない。
        for operation in CATALOG_OPERATIONS {
            let adapter = FakeAdapter::new();
            let result = read(
                &adapter,
                operation,
                json!({ "offset": 1, "limit": 1, "snapshot_revision": REVISION - 1 }),
            )
            .unwrap_or_else(|error| panic!("{operation:?} が拒否されました: {error:?}"));

            assert_eq!(
                result["page"]["offset"], 1,
                "{operation:?} が 2 ページ目を返していません"
            );
        }
    }

    #[test]
    fn catalog_pages_report_the_revision_of_the_enumeration() {
        // 照合しないことと、ページのメタ情報へ何を載せるかは別である。0 のような
        // 固定値は実在し得る revision と区別が付かない。
        for operation in CATALOG_OPERATIONS {
            let adapter = FakeAdapter::new();
            let result = read(&adapter, operation, json!({})).unwrap();
            assert_eq!(
                result["page"]["snapshot_revision"], REVISION,
                "{operation:?}"
            );
        }
    }

    #[test]
    fn the_object_alias_listing_does_not_verify_the_snapshot_revision() {
        // 前ページが返した値をそのまま送り返しても拒否されない。検証済みの
        // 要求をそのまま渡すと落ちる。
        let adapter = FakeAdapter::new();
        let first = read(
            &adapter,
            ReadOperation::ListObjectAliases,
            json!({ "offset": 0, "limit": 1 }),
        )
        .unwrap();
        let returned = first["page"]["snapshot_revision"].clone();

        let second = read(
            &adapter,
            ReadOperation::ListObjectAliases,
            json!({ "offset": 1, "limit": 1, "snapshot_revision": returned }),
        )
        .unwrap();

        assert_eq!(second["page"]["offset"], 1);
        assert_eq!(second["items"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn an_advanced_revision_does_not_reject_the_second_page_of_object_aliases() {
        // 上と対になる。上は「照合しない」を、こちらは「照合しないことが実際に
        // 効く」を見る。フェイクは呼ぶたびに revision を進める。
        let adapter = FakeAdapter::new();
        let first = read(
            &adapter,
            ReadOperation::ListObjectAliases,
            json!({ "offset": 0, "limit": 1 }),
        )
        .unwrap();
        let second = read(
            &adapter,
            ReadOperation::ListObjectAliases,
            json!({ "offset": 1, "limit": 1, "snapshot_revision": REVISION }),
        )
        .unwrap();

        assert_eq!(first["page"]["snapshot_revision"], REVISION);
        assert_eq!(second["page"]["snapshot_revision"], REVISION + 1);
        assert_eq!(second["page"]["offset"], 1);
    }

    #[test]
    fn the_object_alias_listing_filters_by_label() {
        let adapter = FakeAdapter::new();
        let result = read(
            &adapter,
            ReadOperation::ListObjectAliases,
            json!({ "label": "テロップ集" }),
        )
        .unwrap();

        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        assert_eq!(result["items"][0]["name"], "テロップ");
        assert_eq!(result["page"]["total_count"], 1);
    }

    #[test]
    fn an_unusable_label_is_invalid_argument_without_reading() {
        // 種別まで固定する。コードだけを見ると、NUL と長さ超過が同じ応答に
        // 畳まれても気付けない。
        for (label, reason) in [
            (json!("\u{0}"), "contains_nul"),
            (json!("あ".repeat(1025)), "too_long"),
        ] {
            let adapter = FakeAdapter::new();
            let error = read(
                &adapter,
                ReadOperation::ListObjectAliases,
                json!({ "label": label }),
            )
            .unwrap_err();

            assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
            assert_eq!(error.details["reason"], json!(reason), "{label}");
            assert!(!error.retryable, "{label}");
            assert!(adapter.calls().is_empty(), "{label}");
        }
    }

    #[test]
    fn list_fonts_returns_the_registered_names() {
        let adapter = FakeAdapter::new();
        let result = read(&adapter, ReadOperation::ListFonts, json!({})).unwrap();

        assert_eq!(result["items"], json!(fake_fonts()));
        assert_eq!(result["page"]["total_count"], fake_fonts().len());
    }

    #[test]
    fn list_palettes_returns_the_current_name_and_the_colors() {
        let adapter = FakeAdapter::new();
        let result = read(&adapter, ReadOperation::ListPalettes, json!({})).unwrap();

        assert_eq!(result["current"], "[標準.既定]");
        assert_eq!(
            result["items"][0]["colors"].as_array().unwrap().len(),
            PALETTE_COLOR_COUNT
        );
        assert_eq!(result["items"][0]["colors"][0]["a"], 255);
    }

    #[test]
    fn list_modules_filters_by_type() {
        let adapter = FakeAdapter::new();
        let result = read(
            &adapter,
            ReadOperation::ListModules,
            json!({ "module_type": "plugin_input" }),
        )
        .unwrap();

        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        assert_eq!(result["items"][0]["name"], "入力プラグイン");
        assert_eq!(result["items"][0]["module_type"], "plugin_input");
        assert_eq!(result["page"]["total_count"], 1);
    }

    #[test]
    fn list_modules_rejects_a_type_it_cannot_name() {
        // 絞り込みは閉じた集合に対する等値判定である。名乗れない値を受けると、
        // 0 件が「そういう種別が無い」のか「綴りを間違えた」のか区別できない。
        let adapter = FakeAdapter::new();
        let error = read(
            &adapter,
            ReadOperation::ListModules,
            json!({ "module_type": "script_unknown" }),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(adapter.calls().is_empty());
    }

    #[test]
    fn the_module_information_never_reaches_the_log() {
        // 説明文は秘匿の対象ではないが、ローカルのログへは残さない。応答の
        // 組み立てを記録へ写す変更が入れば、ここで現れる。
        let logs = crate::test_support::capture_logs(|| {
            let adapter = FakeAdapter::new();
            let result = read(&adapter, ReadOperation::ListModules, json!({})).unwrap();
            assert_eq!(
                result["items"][0]["information"], "標準搭載",
                "説明文が応答へ載っていません"
            );
        });

        for module in fake_modules() {
            assert!(
                !logs.contains(&module.information),
                "説明文がログへ出ています: {logs}"
            );
        }
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
                RequestDeadline::Within(Instant::now() + read_timeout()),
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

    /// 各 operation が受理しない params。
    fn malformed_params_of_all_operations() -> Vec<(ReadOperation, Value)> {
        all_operations()
            .into_iter()
            .map(|(operation, params)| {
                let mut params = params;
                params
                    .as_object_mut()
                    .unwrap()
                    .insert("future".to_string(), json!(1));
                (operation, params)
            })
            .collect()
    }

    #[test]
    fn invalid_params_are_rejected_regardless_of_the_lifecycle_state() {
        // 要求内容の誤りは状態に依存しない。受付判定を先に通すと、解消しない
        // 誤りが再試行を促す host_busy として返ってしまう。
        for state in [
            InstanceState::Starting,
            InstanceState::Draining,
            InstanceState::Gone,
            InstanceState::Unknown("future".to_string()),
        ] {
            for (operation, params) in malformed_params_of_all_operations() {
                let adapter = FakeAdapter::new();
                let error = execute_read(
                    &adapter,
                    &state,
                    operation,
                    &params,
                    RequestDeadline::Within(Instant::now() + read_timeout()),
                )
                .unwrap_err();

                assert_eq!(
                    error.code,
                    ErrorCode::InvalidArgument,
                    "{state} の {operation:?} が状態由来のエラーで返りました"
                );
                assert!(adapter.calls().is_empty(), "{state}: {operation:?}");
            }
        }
    }

    #[test]
    fn invalid_params_are_rejected_before_the_deadline_is_evaluated() {
        // 期限超過は再試行可能として返る。要求内容の誤りをその後ろに置くと、
        // 解消しない誤りが再試行可能なエラーに化ける。
        for (operation, params) in malformed_params_of_all_operations() {
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
                ErrorCode::InvalidArgument,
                "{operation:?} が期限超過として返りました"
            );
        }
    }

    #[test]
    fn page_and_filter_violations_are_rejected_regardless_of_the_state() {
        let cases = [
            (
                ReadOperation::ListLayers,
                json!({ "expected_scene_id": SCENE_ID, "limit": 0 }),
            ),
            (
                ReadOperation::ListObjects,
                json!({
                    "expected_scene_id": SCENE_ID,
                    "filter": { "layer_min": 2, "layer_max": 1 },
                }),
            ),
            (ReadOperation::ListAvailableEffects, json!({ "limit": 201 })),
        ];

        for (operation, params) in cases {
            let adapter = FakeAdapter::new();
            let error = execute_read(
                &adapter,
                &InstanceState::Starting,
                operation,
                &params,
                RequestDeadline::Within(Instant::now() + read_timeout()),
            )
            .unwrap_err();

            assert_eq!(
                error.code,
                ErrorCode::InvalidArgument,
                "{operation:?} が起動処理中に状態由来のエラーで返りました"
            );
            assert!(adapter.calls().is_empty(), "{operation:?}");
        }
    }

    #[test]
    fn pong_carries_the_project_state_in_every_state() {
        // 生存確認は状態を問わず受け付ける。プロジェクトの状態は SDK に触れずに
        // 読めるため、受付できない状態でも載せられる。
        let instance_id = InstanceId::new_v4();
        for state in [
            InstanceState::Starting,
            InstanceState::Ready,
            InstanceState::Busy,
            InstanceState::Draining,
        ] {
            let adapter = FakeAdapter::new();
            let result = pong_result(instance_id, state.clone(), &adapter);

            assert_eq!(result.instance_id, instance_id);
            assert_eq!(result.state, state);
            let project = result.project.expect("project が載っていません");
            assert_eq!(project.epoch, EPOCH);
            assert_eq!(project.revision, REVISION);
            assert!(project.modified);
            assert_eq!(adapter.calls(), vec!["project_status"]);
        }
    }

    #[test]
    fn pong_does_not_report_a_scene() {
        // シーンは編集ハンドルを介してしか読めず、生存確認を受け付ける全ての
        // 状態でそれを呼べるとは限らない。読み取り口へも問い合わせない。
        let adapter = FakeAdapter::new();
        let result = pong_result(InstanceId::new_v4(), InstanceState::Ready, &adapter);

        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value.get("scene"), None);
        assert_eq!(
            adapter.calls(),
            vec!["project_status"],
            "生存確認が読み取りを行いました"
        );
    }

    #[test]
    fn timeouts_match_the_intended_budget() {
        // 読み取りが実行の上限まで走っても、応答送信の持ち時間が要求元の
        // 要求フェーズ予算の内側に残る。ここが崩れると、完了した読み取りを
        // 誰も待っていない窓へ送ることになる。
        let read = read_timeout();
        let edit = edit_timeout();
        let write = write_timeout();
        let handshake = handshake_timeout();
        // 要求元の予算は倍率を掛けない一式から採る。
        let server = ScaledBudgets::unscaled();
        let headroom = server.transport_headroom();
        let read_request = server.server_request_phase(RequestBudgetKind::Read);
        let edit_request = server.server_request_phase(RequestBudgetKind::Edit);
        let resolve = server.server_resolve();
        assert!(
            read + write + headroom <= read_request,
            "読み取り {read:?} と送信 {write:?} が要求フェーズ予算 {read_request:?} に収まらない"
        );

        // 編集が実行の上限まで走っても、応答送信の持ち時間が編集要求フェーズ
        // 予算の内側に残る。編集は結果を破棄しないため、この余地が無いと
        // 応答を送り切れないまま接続が切れ得る。
        assert!(
            edit + write + headroom <= edit_request,
            "編集 {edit:?} と送信 {write:?} が編集要求フェーズ予算 {edit_request:?} に収まらない"
        );

        // handshake が解決フェーズの予算を使い切ると、続く ping の往復に
        // 持ち時間が残らず、応答している接続が期限超過として扱われる。
        assert!(
            handshake + write + headroom <= resolve,
            "handshake {handshake:?} と ping 応答 {write:?} が解決フェーズ予算 {resolve:?} に収まらない"
        );

        // 接続を保持する上限（REQUEST_IDLE_TIMEOUT）はここで主張しない。掛かる
        // のは要求フレームの到着待ちだけであり、要求の処理時間を含まないため、
        // 要求フェーズの予算と比べる量ではない。比べると、長い予算を持つ
        // operation を足すたびに、無関係なこの値を引き上げる圧力が生まれる。
        // この値はそのまま「沈黙したクライアントが待受を占有できる時間」であり、
        // 引き上げてよい理由が要求の処理時間の側から来ることはない。

        // 再試行案内の設計値。変えると要求元との取り決めが変わるため、
        // 値そのものを主張する。
        assert_eq!(HOST_BUSY_RETRY_AFTER_MS, 500);
    }

    #[test]
    fn every_operation_draws_the_execution_budget_of_its_kind() {
        // 上限を引く経路は要求処理に 1 つしかない。ここが operation ごとに
        // 意図どおりの値を返すことが、全 operation の期限判定の根拠になる。
        //
        // 引いた上限が期限の判定へ実際に効いていることまで見る。要求が期限を
        // 運ばなければ、採用される期限は operation ごとの上限そのものになる。
        let now = Instant::now();
        let deadline = |operation| resolve_execution_deadline(now, NOW_UNIX_MS, operation, None);

        assert_eq!(execution_timeout(Operation::Ping), write_timeout());
        assert_eq!(
            deadline(Operation::Ping),
            RequestDeadline::Within(now + write_timeout())
        );

        for operation in ReadOperation::ALL {
            assert_eq!(
                deadline(Operation::Read(operation)),
                RequestDeadline::Within(now + read_timeout()),
                "{} が読み取りの上限から外れました",
                operation.as_str()
            );
        }

        for operation in EditOperation::ALL {
            let expected = if operation == EditOperation::ApplyBatch {
                batch_timeout()
            } else {
                edit_timeout()
            };
            assert_eq!(
                deadline(Operation::Edit(operation)),
                RequestDeadline::Within(now + expected),
                "{} が編集の上限から外れました",
                operation.as_str()
            );
        }

        for operation in aviutl2_mcp_core::RenderOperation::ALL {
            assert_eq!(
                deadline(Operation::Render(operation)),
                RequestDeadline::Within(now + render_timeout()),
                "{} がレンダリングの上限から外れました",
                operation.as_str()
            );
        }
    }

    #[test]
    fn admit_request_accepts_only_serviceable_states() {
        for state in [InstanceState::Ready, InstanceState::Busy] {
            assert_eq!(admit_request(&state), Ok(()), "{state} が拒否されました");
        }

        for state in [
            InstanceState::Starting,
            InstanceState::Draining,
            InstanceState::Unknown("future".to_string()),
        ] {
            let error = admit_request(&state).unwrap_err();
            assert_eq!(error.code, ErrorCode::HostBusy, "{state} が受理されました");
            assert!(error.retryable);
            assert_eq!(error.details["retry_after_ms"], 500);
        }
    }

    #[test]
    fn gone_instance_is_not_advised_to_retry() {
        // 終了済みのインスタンスは同じ相手として戻らない。再試行の間隔を案内すると
        // 待てば復活するかのように読める。
        let error = admit_request(&InstanceState::Gone).unwrap_err();
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
            || ReadError::FingerprintMismatch {
                current_object: Box::new(crate::test_support::sample_object_summary()),
            },
            || ReadError::ObjectNotFound {
                detected_by: "find_object",
            },
            || ReadError::AmbiguousObject { candidate_count: 2 },
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
        // 読み取りは期限内で終わり、要求の残りは 500 ミリ秒。送信上限より短いので
        // 残りを採る。
        assert_eq!(
            decide_send(
                now,
                NOW_UNIX_MS,
                RequestDeadline::Within(now + Duration::from_secs(4)),
                Some((NOW_UNIX_MS + 500) as u64),
            ),
            SendDecision::Send(now + Duration::from_millis(500))
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
            SendDecision::Send(now + write_timeout())
        );
        assert_eq!(
            decide_send(
                now,
                NOW_UNIX_MS,
                RequestDeadline::Within(now + Duration::from_secs(4)),
                Some((NOW_UNIX_MS + 60_000) as u64),
            ),
            SendDecision::Send(now + write_timeout())
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
            SendDecision::Send(now + write_timeout())
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
        assert_eq!(response.deadline, now + write_timeout());
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
            assert_eq!(response.deadline, now + write_timeout());
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
            Some((NOW_UNIX_MS + 500) as u64),
            Ok(result.clone()),
        );

        assert_eq!(response.outcome.unwrap(), result);
        assert!(!response.discarded);
        assert_eq!(response.deadline, now + Duration::from_millis(500));
    }
}

#[cfg(test)]
mod edit_tests {
    use super::*;
    use crate::edit::error::RollbackOutcome;
    use aviutl2_mcp_core::{
        EditOutcome, FiniteF64, GridBpmOutcome, LayerInfo, LayerStateOutcome, MAX_GRID_BPM_ENTRIES,
        MAX_ITEM_VALUE_BYTES, MAX_PATH_UTF16_UNITS, ObjectFingerprintInput, ObjectSectionsOutcome,
        ObjectSummary, RequestBudgetKind, SceneInfo, SceneSettingsOutcome, SectionRange,
        SelectionField, SelectionState, SetLayerStateParams,
    };
    use serde_json::json;
    use std::sync::Mutex;

    const EPOCH: &str = "9d0a5f4e-2f47-4a13-9a5e-1e2f3a4b5c6d";
    const SCENE_ID: i32 = 0;

    /// 編集口の代わりに定型データを返す実装。
    ///
    /// 呼ばれた operation を記録するため、受付判定や params の検証で弾かれた
    /// 要求が編集へ進んでいないことを確かめられる。
    struct FakeEditAdapter {
        calls: Mutex<Vec<&'static str>>,
    }

    impl FakeEditAdapter {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        fn enter(&self, call: &'static str) -> EditOutcome {
            self.calls.lock().unwrap().push(call);
            EditOutcome::object_changed(EPOCH, 1, fake_summary())
        }

        fn enter_sections(&self, call: &'static str) -> ObjectSectionsOutcome {
            self.calls.lock().unwrap().push(call);
            ObjectSectionsOutcome {
                project_epoch: EPOCH.to_string(),
                project_revision: 1,
                object: fake_summary(),
                sections: vec![SectionRange {
                    start: 100,
                    end: 200,
                }],
            }
        }
    }

    fn fake_summary() -> ObjectSummary {
        ObjectSummary::new(
            EPOCH,
            ObjectFingerprintInput {
                scene_id: SCENE_ID,
                layer: 1,
                frame_start: 100,
                frame_end: 200,
                name: None,
                alias: "[1:100]",
            },
        )
    }

    /// 対象を指す effect セレクター。解決はフェイクが行わないため値は任意でよい。
    fn fake_effect_selector() -> Value {
        json!({
            "object": fake_summary().selector,
            "effect_name": "動画ファイル",
            "effect_index": 0,
            "fingerprint": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        })
    }

    impl EditAdapter for FakeEditAdapter {
        fn create_object(&self, _: &CreateObjectParams) -> Result<EditOutcome, EditError> {
            Ok(self.enter("create_object"))
        }

        fn move_object(&self, _: &MoveObjectParams) -> Result<EditOutcome, EditError> {
            Ok(self.enter("move_object"))
        }

        fn delete_object(&self, _: &DeleteObjectParams) -> Result<EditOutcome, EditError> {
            Ok(self.enter("delete_object"))
        }

        fn set_object_name(&self, _: &SetObjectNameParams) -> Result<EditOutcome, EditError> {
            Ok(self.enter("set_object_name"))
        }

        fn create_object_section(
            &self,
            _: &CreateObjectSectionParams,
        ) -> Result<ObjectSectionsOutcome, EditError> {
            Ok(self.enter_sections("create_object_section"))
        }

        fn delete_object_section(
            &self,
            _: &DeleteObjectSectionParams,
        ) -> Result<ObjectSectionsOutcome, EditError> {
            Ok(self.enter_sections("delete_object_section"))
        }

        fn move_object_section(
            &self,
            _: &MoveObjectSectionParams,
        ) -> Result<ObjectSectionsOutcome, EditError> {
            Ok(self.enter_sections("move_object_section"))
        }

        fn set_grid_bpm(&self, _: &SetGridBpmParams) -> Result<GridBpmOutcome, EditError> {
            self.calls.lock().unwrap().push("set_grid_bpm");
            Ok(GridBpmOutcome {
                project_epoch: EPOCH.to_string(),
                project_revision: 1,
                entries: Vec::new(),
            })
        }

        fn set_object_item(&self, _: &SetObjectItemParams) -> Result<EditOutcome, EditError> {
            Ok(self.enter("set_object_item"))
        }

        fn add_effect(&self, _: &AddEffectParams) -> Result<EditOutcome, EditError> {
            Ok(self.enter("add_effect"))
        }

        fn delete_effect(&self, _: &DeleteEffectParams) -> Result<EditOutcome, EditError> {
            Ok(self.enter("delete_effect"))
        }

        fn set_effect_enabled(&self, _: &SetEffectEnabledParams) -> Result<EditOutcome, EditError> {
            Ok(self.enter("set_effect_enabled"))
        }

        fn set_scene_settings(
            &self,
            _: &SetSceneSettingsParams,
        ) -> Result<SceneSettingsOutcome, EditError> {
            self.calls.lock().unwrap().push("set_scene_settings");
            Ok(SceneSettingsOutcome {
                project_epoch: EPOCH.to_string(),
                project_revision: 1,
                scene: SceneInfo {
                    id: SCENE_ID,
                    name: Some("本編".to_string()),
                    width: 1280,
                    height: 720,
                    fps: FiniteF64::try_new(30.0),
                    fps_rate: 30,
                    fps_scale: 1,
                    sample_rate: 48000,
                },
                observed_after_edit: true,
                non_undoable: true,
            })
        }

        fn set_layer_state(&self, _: &SetLayerStateParams) -> Result<LayerStateOutcome, EditError> {
            self.calls.lock().unwrap().push("set_layer_state");
            Ok(LayerStateOutcome {
                project_epoch: EPOCH.to_string(),
                project_revision: 1,
                layer: LayerInfo {
                    index: 1,
                    name: Some("背景".to_string()),
                    enabled: true,
                    locked: false,
                    object_count: 0,
                },
            })
        }

        fn apply_batch(
            &self,
            _: &aviutl2_mcp_core::ApplyBatchParams,
        ) -> Result<aviutl2_mcp_core::BatchOutcome, EditError> {
            self.calls.lock().unwrap().push("apply_batch");
            Ok(aviutl2_mcp_core::BatchOutcome {
                project_epoch: EPOCH.to_string(),
                project_revision: 1,
                results: Vec::new(),
            })
        }

        fn set_selection(&self, _: &SetSelectionParams) -> Result<SelectionState, EditError> {
            self.calls.lock().unwrap().push("set_selection");
            Ok(SelectionState::observed(
                EPOCH,
                1,
                aviutl2_mcp_core::ObservedSelection {
                    cursor: aviutl2_mcp_core::Cursor { frame: 0, layer: 0 },
                    selected_range: None,
                    focus: None,
                    display: aviutl2_mcp_core::DisplayRange {
                        frame_start: 0,
                        layer_start: 0,
                        frame_num: 0,
                        layer_num: 0,
                    },
                },
                vec![SelectionField::Cursor],
                Vec::new(),
            ))
        }
    }

    /// 有効な選択状態の変更 params。
    fn selection_params() -> Value {
        json!({
            "expected_scene_id": SCENE_ID,
            "cursor": { "layer": 0, "frame": 0 },
            "expected_project_epoch": EPOCH,
        })
    }

    /// operation ごとの、現在の形の要求 params を引く。実行口を持たない
    /// operation は `None`。
    ///
    /// **`_` を使わない網羅 match で書く。** 編集 operation を足すとここが
    /// コンパイルエラーになるため、要求の形を確かめる一連のテストから漏れる
    /// ことがない。手書きの一覧にすると、足し忘れても全て緑のまま通ってしまう。
    fn current_request(operation: EditOperation) -> Option<Value> {
        Some(match operation {
            EditOperation::CreateObject => json!({
                "source": { "type": "object_alias", "alias": "[1:100]" },
                "placement": { "scene_id": SCENE_ID, "layer": 1, "frame": 0 },
                "expected_project_epoch": EPOCH,
            }),
            EditOperation::MoveObject => json!({
                "selector": fake_summary().selector,
                "destination": { "layer": 1, "frame": 300 },
            }),
            EditOperation::DeleteObject => json!({ "selector": fake_summary().selector }),
            EditOperation::SetObjectName => json!({
                "selector": fake_summary().selector,
                "name": "名前",
            }),
            EditOperation::SetObjectItem => json!({
                "selector": fake_effect_selector(),
                "item": "X",
                "value": { "type": "integer", "value": 1 },
            }),
            EditOperation::AddEffect => json!({
                "object": fake_summary().selector,
                "effect_name": "ぼかし",
            }),
            EditOperation::DeleteEffect => json!({ "selector": fake_effect_selector() }),
            EditOperation::SetEffectEnabled => json!({
                "selector": fake_effect_selector(),
                "enabled": true,
            }),
            EditOperation::SetLayerState => json!({
                "expected_scene_id": SCENE_ID,
                "layer": 1,
                "name": { "type": "set", "name": "背景" },
                "expected_project_epoch": EPOCH,
            }),
            EditOperation::SetSelection => selection_params(),
            EditOperation::CreateObjectSection => json!({
                "selector": fake_summary().selector,
                "frame": 150,
            }),
            EditOperation::DeleteObjectSection => json!({
                "selector": fake_summary().selector,
                "section": 1,
            }),
            EditOperation::MoveObjectSection => json!({
                "selector": fake_summary().selector,
                "section": 1,
                "frame": 160,
            }),
            EditOperation::SetGridBpm => json!({
                "expected_scene_id": SCENE_ID,
                "entries": [{ "tempo": 120.0, "beat": 4, "start": 0.0, "offset": 0.0 }],
                "expected_project_epoch": EPOCH,
            }),
            EditOperation::SetSceneSettings => json!({
                "expected_scene_id": SCENE_ID,
                "name": "本編",
                "size": { "width": 1280, "height": 720 },
                "sample_rate": 48000,
                "expected_project_epoch": EPOCH,
            }),
            EditOperation::ApplyBatch => batch_params(),
        })
    }

    /// 移動 1 件だけの一括適用 params。
    fn batch_params() -> Value {
        json!({
            "operations": [{
                "type": "move_object",
                "selector": fake_summary().selector,
                "destination": { "layer": 1, "frame": 300 },
            }],
        })
    }

    /// 要求を復号し、成功したら params を JSON へ写して返す。
    fn decode_request(operation: EditOperation, params: &Value) -> Result<Value, ErrorObject> {
        let request = decode_edit_request(operation, params)?;
        let encoded = match &request {
            EditRequest::CreateObject(params) => serde_json::to_value(params),
            EditRequest::MoveObject(params) => serde_json::to_value(params),
            EditRequest::DeleteObject(params) => serde_json::to_value(params),
            EditRequest::SetObjectName(params) => serde_json::to_value(params),
            EditRequest::SetObjectItem(params) => serde_json::to_value(params),
            EditRequest::AddEffect(params) => serde_json::to_value(params),
            EditRequest::DeleteEffect(params) => serde_json::to_value(params),
            EditRequest::SetEffectEnabled(params) => serde_json::to_value(params),
            EditRequest::SetLayerState(params) => serde_json::to_value(params),
            EditRequest::SetSelection(params) => serde_json::to_value(params),
            EditRequest::CreateObjectSection(params) => serde_json::to_value(params),
            EditRequest::DeleteObjectSection(params) => serde_json::to_value(params),
            EditRequest::MoveObjectSection(params) => serde_json::to_value(params),
            EditRequest::SetGridBpm(params) => serde_json::to_value(params),
            EditRequest::SetSceneSettings(params) => serde_json::to_value(params),
            EditRequest::ApplyBatch(params) => serde_json::to_value(params),
        };
        Ok(encoded.expect("params は直列化できる"))
    }

    /// JSON の中の全てのオブジェクト selector へ手を入れる。
    ///
    /// 要求の形ごとに selector の位置を知らずに済むよう、木を辿って
    /// `project_epoch` を持つオブジェクトを selector と見なす。
    fn for_each_object_selector(
        value: &mut Value,
        apply: &impl Fn(&mut serde_json::Map<String, Value>),
    ) {
        match value {
            Value::Object(map) => {
                if map.contains_key("project_epoch") {
                    apply(map);
                }
                for nested in map.values_mut() {
                    for_each_object_selector(nested, apply);
                }
            }
            Value::Array(items) => {
                for item in items {
                    for_each_object_selector(item, apply);
                }
            }
            _ => {}
        }
    }

    /// JSON の中の全ての effect selector へ手を入れる。
    fn for_each_effect_selector(
        value: &mut Value,
        apply: &impl Fn(&mut serde_json::Map<String, Value>),
    ) {
        match value {
            Value::Object(map) => {
                if map.contains_key("effect_index") && map.contains_key("object") {
                    apply(map);
                }
                for nested in map.values_mut() {
                    for_each_effect_selector(nested, apply);
                }
            }
            Value::Array(items) => {
                for item in items {
                    for_each_effect_selector(item, apply);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn every_edit_request_follows_the_selector_and_unknown_field_table() {
        // 1 つの operation で通しても、他が違う扱いなら気付けないため、全
        // operation を網羅 match から引いて同じ表に掛ける。
        for operation in EditOperation::ALL {
            let name = operation.as_str();
            let Some(current) = current_request(operation) else {
                continue;
            };
            decode_request(operation, &current)
                .unwrap_or_else(|error| panic!("{name} の現在の形が拒否されました: {error:?}"));

            // セレクターは算出方式を運ばないが、往復型なので名乗る指定も
            // 拒否せず、値を解釈せずに捨てる。
            let mut with_algorithm = current.clone();
            let insert = |map: &mut serde_json::Map<String, Value>| {
                map.insert("fingerprint_algorithm".to_string(), json!("sha256-raw-v1"));
            };
            for_each_object_selector(&mut with_algorithm, &insert);
            for_each_effect_selector(&mut with_algorithm, &insert);
            assert!(
                decode_request(operation, &with_algorithm).is_ok(),
                "{name} がセレクターの算出方式を拒否しました"
            );

            // 未知フィールドは拒否する。
            let mut unknown = current.clone();
            unknown
                .as_object_mut()
                .unwrap()
                .insert("unknown_field".to_string(), json!(1));
            let error = decode_request(operation, &unknown)
                .expect_err(&format!("{name} が未知フィールドを受理しました"));
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{name}");

            // 入れ子の未知フィールドも拒否する。往復型は対象から外す。
            for key in current.as_object().expect("params は object").keys() {
                if is_round_trip_field(key) {
                    continue;
                }
                let mut nested = current.clone();
                let Some(inner) = nested[key].as_object_mut() else {
                    continue;
                };
                inner.insert("unknown_field".to_string(), json!(1));
                let error = decode_request(operation, &nested)
                    .expect_err(&format!("{name} の {key} が未知フィールドを受理しました"));
                assert_eq!(error.code, ErrorCode::InvalidArgument, "{name}.{key}");
            }
        }
    }

    /// 応答が返した値をそのまま送り返す往復型のフィールドか。
    ///
    /// 往復型は応答へ optional field が増えても往復が壊れないよう、未知
    /// フィールドを拒否しない。
    fn is_round_trip_field(key: &str) -> bool {
        matches!(key, "selector" | "object" | "value")
    }

    #[test]
    fn only_the_requests_without_a_selector_require_an_expected_epoch() {
        // 前提の epoch を持つのは、対象を指すセレクターを持たない要求だけである。
        // 持つ要求ではその欠落が拒否になり、持たない要求ではフィールド自体が無い。
        let mut carriers = Vec::new();
        for operation in EditOperation::ALL {
            let Some(current) = current_request(operation) else {
                continue;
            };
            let mut without = current.clone();
            if without
                .as_object_mut()
                .unwrap()
                .remove("expected_project_epoch")
                .is_none()
            {
                continue;
            }
            carriers.push(operation.as_str());
            let error = decode_request(operation, &without).expect_err(&format!(
                "{} が前提の epoch なしで受理されました",
                operation.as_str()
            ));
            assert_eq!(error.code, ErrorCode::InvalidArgument);
        }

        assert_eq!(
            carriers,
            vec![
                EditOperation::CreateObject.as_str(),
                EditOperation::SetLayerState.as_str(),
                EditOperation::SetSelection.as_str(),
                EditOperation::SetGridBpm.as_str(),
                EditOperation::SetSceneSettings.as_str()
            ]
        );
    }

    #[test]
    fn every_edit_operation_is_routed_from_its_name() {
        for operation in EditOperation::ALL {
            assert_eq!(
                classify_operation(operation.as_str()).unwrap(),
                Operation::Edit(operation),
                "{} が編集へ振り分けられていません",
                operation.as_str()
            );
        }
    }

    #[test]
    fn only_names_outside_every_family_are_unsupported() {
        // 分類できる名前には必ず実行口がある。未対応として返るのは、どの族にも
        // 属さない名前だけである。
        for operation in EditOperation::ALL
            .map(KnownOperation::Edit)
            .into_iter()
            .chain(ReadOperation::ALL.map(KnownOperation::Read))
            .chain(RenderOperation::ALL.map(KnownOperation::Render))
        {
            assert!(
                classify_operation(operation.as_str()).is_ok(),
                "{} が未対応として返りました",
                operation.as_str()
            );
        }

        for name in ["apply_batches", "render_frames", "future_operation"] {
            let error = classify_operation(name).expect_err(&format!("{name} が受理されました"));
            assert_eq!(error.code, ErrorCode::UnsupportedOperation, "{name}");
            assert!(!error.retryable, "{name}");
        }
    }

    #[test]
    fn the_request_table_leaves_out_no_operation() {
        // 網羅 match は operation の追加を止めるが、既存の枝を除外へ書き換えても
        // 止まらない。表から外れているものが 1 つも無いことを固定することで、
        // 除外を増やせばここが落ちる。
        let excluded: Vec<&str> = EditOperation::ALL
            .into_iter()
            .filter(|operation| current_request(*operation).is_none())
            .map(EditOperation::as_str)
            .collect();

        assert!(
            excluded.is_empty(),
            "要求の形の表から外れています: {excluded:?}"
        );
    }

    #[test]
    fn params_are_decoded_before_the_lifecycle_state_is_checked() {
        // 起動処理中でも、要求内容の誤りは要求の誤りとして返す。状態由来の
        // 再試行可能なエラーで返すと、解消しない再試行を促してしまう。
        let adapter = FakeEditAdapter::new();
        let error = execute_edit(
            &adapter,
            &InstanceState::Starting,
            EditOperation::SetSelection,
            &json!({ "expected_scene_id": SCENE_ID }),
            within(),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(adapter.calls().is_empty());
    }

    /// 期限内の判定。
    fn within() -> RequestDeadline {
        RequestDeadline::Within(Instant::now() + Duration::from_secs(1))
    }

    /// 要求内容の誤りを、状態と期限のどちらへ先に掛けても崩れない組。
    ///
    /// 受付判定を先に通すと、解消しない誤りが再試行を促す `host_busy` として
    /// 返る。期限判定を先に通すと、同じ誤りが再試行可能な `timeout` に化ける。
    /// **どちらの順序も塞ぐ。** 片方だけを見ていると、もう一方へ入れ替える
    /// 変更が素通りする。
    fn misordering_cases() -> [(InstanceState, RequestDeadline, &'static str); 2] {
        [
            (InstanceState::Starting, within(), "起動処理中"),
            (InstanceState::Ready, RequestDeadline::Exceeded, "期限超過"),
        ]
    }

    #[test]
    fn invalid_edit_params_are_rejected_before_the_state_and_the_deadline() {
        // 全 operation を網羅 match の表から引く。一括適用も単一の編集も、
        // 要求内容の誤りに対する扱いは同じでなければならない。
        for (state, deadline, order) in misordering_cases() {
            for operation in EditOperation::ALL {
                let mut params = current_request(operation).expect("要求の形が表にありません");
                params
                    .as_object_mut()
                    .expect("params は object")
                    .insert("unknown_field".to_string(), json!(1));

                let adapter = FakeEditAdapter::new();
                let error =
                    execute_edit(&adapter, &state, operation, &params, deadline).unwrap_err();

                assert_eq!(
                    error.code,
                    ErrorCode::InvalidArgument,
                    "{order} の {} が要求内容の誤りとして返りませんでした",
                    operation.as_str()
                );
                assert!(
                    adapter.calls().is_empty(),
                    "{order} の {} が編集口へ届きました",
                    operation.as_str()
                );
            }
        }
    }

    #[test]
    fn section_zero_never_reaches_the_edit_adapter() {
        // 区間 0 の開始位置はオブジェクトの開始フレームであって中間点ではない。
        // 対象の状態に依らず常に誤りであるため、編集区間へ入る前に落ちる。
        for (operation, params) in [
            (
                EditOperation::DeleteObjectSection,
                json!({
                    "selector": fake_summary().selector,
                    "section": 0,
                }),
            ),
            (
                EditOperation::MoveObjectSection,
                json!({
                    "selector": fake_summary().selector,
                    "section": 0,
                    "frame": 160,
                }),
            ),
        ] {
            let adapter = FakeEditAdapter::new();
            let error = execute_edit(
                &adapter,
                &InstanceState::Ready,
                operation,
                &params,
                within(),
            )
            .unwrap_err();

            let name = operation.as_str();
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{name}");
            assert_eq!(
                error.details["reason"],
                json!("section_index_out_of_range"),
                "{name}"
            );
            assert!(adapter.calls().is_empty(), "{name} が編集口へ届きました");
        }
    }

    #[test]
    fn a_section_index_of_one_reaches_the_edit_adapter() {
        // 区間の総数との比較は対象の現在の状態を要する。要求内容だけの検証は
        // そこまで見ず、1 以上はそのまま編集口へ届く。
        let adapter = FakeEditAdapter::new();
        execute_edit(
            &adapter,
            &InstanceState::Ready,
            EditOperation::DeleteObjectSection,
            &json!({
                "selector": fake_summary().selector,
                "section": 1,
            }),
            within(),
        )
        .expect("区間番号 1 が編集口へ届きませんでした");
        assert_eq!(adapter.calls(), vec!["delete_object_section"]);
    }

    /// BPM 情報 1 件を要求の形で組み立てる。
    fn grid_bpm_json(tempo: f64, beat: i64, start: f64) -> Value {
        json!({ "tempo": tempo, "beat": beat, "start": start, "offset": 0.0 })
    }

    /// BPM グリッドの置き換え要求を組み立てる。
    fn set_grid_bpm_json(entries: Vec<Value>) -> Value {
        json!({
            "expected_scene_id": SCENE_ID,
            "entries": entries,
            "expected_project_epoch": EPOCH,
        })
    }

    #[test]
    fn an_invalid_grid_bpm_list_never_reaches_the_edit_adapter() {
        // 検証は core の純関数にあり、要求の復号がそれを呼ぶ。呼ばなくなると
        // IPC を直接叩く経路が server と違う要求集合を受理するようになる。
        let over_the_limit = (0..=MAX_GRID_BPM_ENTRIES)
            .map(|index| grid_bpm_json(120.0, 4, index as f64))
            .collect::<Vec<_>>();
        for (label, entries, reason) in [
            ("上限超過", over_the_limit, Value::Null),
            (
                "start の重複",
                vec![grid_bpm_json(120.0, 4, 5.0), grid_bpm_json(90.0, 3, 5.0)],
                json!("duplicate_target"),
            ),
            (
                "範囲外の tempo",
                vec![grid_bpm_json(0.0, 4, 0.0)],
                json!("grid_bpm_out_of_range"),
            ),
            (
                "受け渡せない beat",
                vec![grid_bpm_json(120.0, i64::from(i32::MAX) + 1, 0.0)],
                json!("argument_not_representable"),
            ),
        ] {
            let adapter = FakeEditAdapter::new();
            let error = execute_edit(
                &adapter,
                &InstanceState::Ready,
                EditOperation::SetGridBpm,
                &set_grid_bpm_json(entries),
                within(),
            )
            .unwrap_err();

            assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
            assert_eq!(error.details["reason"], reason, "{label}");
            assert!(adapter.calls().is_empty(), "{label} が編集口へ届きました");
        }
    }

    #[test]
    fn a_valid_grid_bpm_list_reaches_the_edit_adapter() {
        // 拒否だけを固定すると、全ての要求を拒む実装でも緑のまま通る。
        let adapter = FakeEditAdapter::new();
        execute_edit(
            &adapter,
            &InstanceState::Ready,
            EditOperation::SetGridBpm,
            &set_grid_bpm_json(vec![
                grid_bpm_json(120.0, 4, 30.0),
                grid_bpm_json(90.0, 3, 10.0),
            ]),
            within(),
        )
        .expect("正常な一覧が編集口へ届きませんでした");
        assert_eq!(adapter.calls(), vec!["set_grid_bpm"]);
    }

    #[test]
    fn a_request_that_changes_nothing_is_an_invalid_argument() {
        let adapter = FakeEditAdapter::new();
        let error = execute_edit(
            &adapter,
            &InstanceState::Ready,
            EditOperation::SetSelection,
            &json!({
                "expected_scene_id": SCENE_ID,
                "expected_project_epoch": EPOCH,
            }),
            RequestDeadline::Within(Instant::now() + Duration::from_secs(1)),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(adapter.calls().is_empty());
    }

    /// パスの構文検証が拒否する入力と、返るべき失敗の種別名。
    ///
    /// 実機で観測した入力集合をそのまま用い、長さと NUL を足して 7 種すべてを
    /// 覆う。どれも同じ `invalid_argument` で返るため、区別できるのは名前だけ
    /// である。
    fn rejected_path_cases() -> Vec<(String, &'static str, &'static str)> {
        vec![
            (String::new(), "空文字列", "empty_path"),
            ("C:\\movie\0.mp4".to_string(), "NUL", "contains_nul"),
            (
                format!("C:\\{}", "a".repeat(MAX_PATH_UTF16_UNITS)),
                "長さ超過",
                "path_too_long",
            ),
            (
                r"\\.\pipe\aviutl2".to_string(),
                "device namespace",
                "device_namespace",
            ),
            (
                r"\\?\C:\movie.mp4".to_string(),
                "device namespace の別表記",
                "device_namespace",
            ),
            (
                r"C:\movie.mp4:stream".to_string(),
                "代替データストリーム",
                "alternate_data_stream",
            ),
            (r"..\movie.mp4".to_string(), "相対パス", "not_absolute"),
            (
                r"\\server\share\movie.mp4".to_string(),
                "ネットワークパス",
                "unc_path",
            ),
            (
                "//server/share/movie.mp4".to_string(),
                "区切りを揃えたネットワークパス",
                "unc_path",
            ),
            (r"\\server\share".to_string(), "共有そのもの", "unc_path"),
        ]
    }

    #[test]
    fn rejected_paths_never_reach_the_edit_section() {
        // パスの構文は要求元の側でも検証されるが、そこを通らない要求もある。
        // 実行側で弾けなければ、ネットワーク越しの接続や device namespace への
        // 到達をホストへ任せることになる。
        for (path, label, _) in rejected_path_cases() {
            let path = path.as_str();
            for (operation, params) in [
                (
                    EditOperation::CreateObject,
                    json!({
                        "source": { "type": "media_file", "path": path },
                        "placement": { "scene_id": SCENE_ID, "layer": 1, "frame": 0 },
                        "expected_project_epoch": EPOCH,
                    }),
                ),
                (
                    EditOperation::SetObjectItem,
                    json!({
                        "selector": fake_effect_selector(),
                        "item": "ファイル",
                        "value": { "type": "file", "path": path },
                    }),
                ),
            ] {
                let adapter = FakeEditAdapter::new();
                let error = execute_edit(
                    &adapter,
                    &InstanceState::Ready,
                    operation,
                    &params,
                    RequestDeadline::Within(Instant::now() + Duration::from_secs(1)),
                )
                .unwrap_err();

                assert_eq!(
                    error.code,
                    ErrorCode::InvalidArgument,
                    "{label} が {operation:?} で拒否されませんでした"
                );
                assert!(
                    adapter.calls().is_empty(),
                    "{label} が {operation:?} で編集口へ届きました"
                );
            }
        }
    }

    #[test]
    fn rejected_paths_name_the_rule_they_broke() {
        // 7 種はいずれも invalid_argument で返る。名前が無ければ、要求元は
        // 「ローカルへ複製する」「絶対パスにする」「短い場所へ移す」のどれを
        // 取ればよいか説明の文面からしか読めない。
        //
        // メディアファイルからの作成と設定値の書き込みは別の検証を通るが、
        // 同じ入力には同じ名前が返る。
        for (path, label, reason) in rejected_path_cases() {
            let path = path.as_str();
            for (operation, params) in [
                (
                    EditOperation::CreateObject,
                    json!({
                        "source": { "type": "media_file", "path": path },
                        "placement": { "scene_id": SCENE_ID, "layer": 1, "frame": 0 },
                        "expected_project_epoch": EPOCH,
                    }),
                ),
                (
                    EditOperation::SetObjectItem,
                    json!({
                        "selector": fake_effect_selector(),
                        "item": "ファイル",
                        "value": { "type": "file", "path": path },
                    }),
                ),
            ] {
                let error = execute_edit(
                    &FakeEditAdapter::new(),
                    &InstanceState::Ready,
                    operation,
                    &params,
                    within(),
                )
                .unwrap_err();

                assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
                assert_eq!(
                    error.details["reason"],
                    json!(reason),
                    "{label} が {operation:?} で名乗った種別が想定と異なります"
                );
                assert!(
                    !error.details.to_string().contains("movie"),
                    "{label} の補助情報にパスが現れました: {}",
                    error.details
                );
            }
        }
    }

    #[test]
    fn rejected_texts_name_the_rule_they_broke() {
        // 文字列の検証も同じである。空・NUL・制御文字・長さ超過はいずれも
        // invalid_argument であり、要求元が取れる行動だけが異なる。
        let item = |value: String| {
            (
                EditOperation::SetObjectItem,
                json!({
                    "selector": fake_effect_selector(),
                    "item": "文字",
                    "value": { "type": "text", "value": value },
                }),
            )
        };
        let cases = [
            (
                "空文字列",
                "empty",
                (
                    EditOperation::SetLayerState,
                    json!({
                        "expected_scene_id": SCENE_ID,
                        "layer": 1,
                        "name": { "type": "set", "name": "" },
                        "expected_project_epoch": EPOCH,
                    }),
                ),
            ),
            ("NUL", "contains_nul", item("あ\0い".to_string())),
            (
                "制御文字",
                "contains_control",
                item("あ\u{1}い".to_string()),
            ),
            (
                "長さ超過",
                "too_long",
                item("あ".repeat(MAX_ITEM_VALUE_BYTES)),
            ),
        ];
        for (label, reason, (operation, params)) in cases {
            let error = execute_edit(
                &FakeEditAdapter::new(),
                &InstanceState::Ready,
                operation,
                &params,
                within(),
            )
            .unwrap_err();

            assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
            assert_eq!(
                error.details["reason"],
                json!(reason),
                "{label} が名乗った種別が想定と異なります"
            );
            assert!(
                !error.details.to_string().contains('あ'),
                "{label} の補助情報に設定値が現れました: {}",
                error.details
            );
        }
    }

    #[test]
    fn a_batch_gives_the_same_reason_as_the_same_edit_on_its_own() {
        // 一括適用は位置を添えるが、失敗の種別は単独編集と同じ名前で返る。
        // 経路ごとに違う名前を返せば、要求元は一括適用のためだけの分岐を持つ。
        for (path, label, reason) in rejected_path_cases() {
            let operation = json!({
                "type": "set_object_item",
                "selector": fake_effect_selector(),
                "item": "ファイル",
                "value": { "type": "file", "path": path.as_str() },
            });
            let error = execute_edit(
                &FakeEditAdapter::new(),
                &InstanceState::Ready,
                EditOperation::ApplyBatch,
                &json!({ "operations": [operation] }),
                within(),
            )
            .unwrap_err();

            assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
            assert_eq!(error.details["reason"], json!(reason), "{label}");
            assert_eq!(
                error.details["failed_index"],
                json!(0),
                "{label} が落ちた sub-operation の位置を運びませんでした"
            );
        }

        // フォルダも同じパス検証を通る。片方だけを固定すると、種別ごとに
        // 検証を書き分ける形へ戻っても気付けない。
        let error = execute_edit(
            &FakeEditAdapter::new(),
            &InstanceState::Ready,
            EditOperation::ApplyBatch,
            &json!({
                "operations": [{
                    "type": "set_object_item",
                    "selector": fake_effect_selector(),
                    "item": "フォルダ",
                    "value": { "type": "folder", "path": r"..\assets" },
                }],
            }),
            within(),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.details["reason"], json!("not_absolute"));
        assert_eq!(error.details["failed_index"], json!(0));
    }

    #[test]
    fn both_duplicate_checks_name_the_same_fact() {
        // 同じ状態を書き換える組は 2 層で検出する。手前の層は要求内容だけを
        // 見て、奥の層は解決した結果を見る。**文字列として同一のセレクターを
        // 並べた要求——最も素直な入力——は手前の層で落ちる。** ここで名前が
        // 付かなければ、名前で分岐する要求元は稀な入力だけを拾う。
        let move_op = json!({
            "type": "move_object",
            "selector": fake_summary().selector,
            "destination": { "layer": 1, "frame": 300 },
        });
        let from_request = execute_edit(
            &FakeEditAdapter::new(),
            &InstanceState::Ready,
            EditOperation::ApplyBatch,
            &json!({ "operations": [move_op, move_op] }),
            within(),
        )
        .unwrap_err();

        // 奥の層が同じ事実を検出したときの応答。
        let after_resolution = edit_error(EditError::Batch {
            source: Box::new(EditError::DuplicateTarget),
            failed_index: Some(1),
            rollback: RollbackOutcome::NotAttempted,
        });

        assert_eq!(from_request.code, after_resolution.code);
        assert_eq!(from_request.details["reason"], json!("duplicate_target"));
        assert_eq!(
            from_request.details["reason"], after_resolution.details["reason"],
            "2 層の検査が同じ事実に別の名前を付けました"
        );
        assert_eq!(
            from_request.details["failed_index"],
            after_resolution.details["failed_index"]
        );
    }

    #[test]
    fn a_batch_failure_of_the_request_as_a_whole_has_no_reason() {
        // 件数の誤りに対応する単独編集は無い。名前を持たない失敗へ名前を
        // 与えると、要求元は存在しない種別の分岐を書くことになる。
        let error = execute_edit(
            &FakeEditAdapter::new(),
            &InstanceState::Ready,
            EditOperation::ApplyBatch,
            &json!({ "operations": [] }),
            within(),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.details.get("reason"), None, "{:?}", error.details);
    }

    #[test]
    fn a_starting_instance_rejects_a_well_formed_edit() {
        let adapter = FakeEditAdapter::new();
        let error = execute_edit(
            &adapter,
            &InstanceState::Starting,
            EditOperation::SetSelection,
            &selection_params(),
            RequestDeadline::Within(Instant::now() + Duration::from_secs(1)),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::HostBusy);
        assert!(adapter.calls().is_empty());
    }

    #[test]
    fn an_expired_deadline_stops_the_edit_before_it_starts() {
        let adapter = FakeEditAdapter::new();
        let error = execute_edit(
            &adapter,
            &InstanceState::Ready,
            EditOperation::SetSelection,
            &selection_params(),
            RequestDeadline::Exceeded,
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(error.retryable);
        let details = error.details;
        // 実行前に返す timeout だけが「変更は行われていない」と名乗れる。
        assert_eq!(details["change_applied"], json!("no"));
        assert_eq!(details["mutation_origin"], json!("plugin"));
        assert_eq!(details["retry_requires"], json!("resend"));
        assert!(adapter.calls().is_empty(), "SDK を呼ばずに中止していません");
    }

    #[test]
    fn an_edit_within_the_deadline_reaches_the_adapter() {
        let adapter = FakeEditAdapter::new();
        let result = execute_edit(
            &adapter,
            &InstanceState::Ready,
            EditOperation::SetSelection,
            &selection_params(),
            RequestDeadline::Within(Instant::now() + Duration::from_secs(1)),
        )
        .expect("期限内の編集が拒否されました");

        assert_eq!(adapter.calls(), vec!["set_selection"]);
        assert_eq!(result["applied"], json!(["cursor"]));
    }

    #[test]
    fn the_send_budget_is_never_shortened_by_the_request_deadline() {
        // 編集は結果を破棄しないため、送信には常に送信上限をそのまま充てる。
        // 期限際まで掛かった編集の送信に数ミリ秒しか残らないと、適用済みの
        // 変更が要求元からは無応答に見える。
        let now = Instant::now();
        assert_eq!(retained_send_deadline(now), now + write_timeout());

        // 読み取りは要求の残り時間で縮める。捨ててよい結果と捨ててはいけない
        // 結果の差がここに出る。
        assert_eq!(
            resolve_request_deadline(
                now,
                NOW_UNIX_MS,
                write_timeout(),
                Some((NOW_UNIX_MS + 200) as u64)
            ),
            RequestDeadline::Within(now + Duration::from_millis(200))
        );
    }

    #[test]
    fn a_batch_is_given_its_own_execution_budget() {
        // 一括適用の費用は変更の件数だけでは決まらない。単一の編集と同じ上限に
        // 落ちると、事前解決相だけで尽きる要求が実行前の期限超過になる。
        assert_eq!(
            execution_timeout(Operation::Edit(EditOperation::ApplyBatch)),
            batch_timeout()
        );
        assert_ne!(batch_timeout(), edit_timeout());

        // 一括適用以外の編集は編集の上限のままである。
        for operation in EditOperation::ALL {
            if operation == EditOperation::ApplyBatch {
                continue;
            }
            assert_eq!(
                execution_timeout(Operation::Edit(operation)),
                edit_timeout(),
                "{} が編集の上限から外れました",
                operation.as_str()
            );
        }

        // 一括適用が実行の上限まで走っても、応答送信の持ち時間が一括適用の
        // 要求フェーズ予算の内側に残る。一括適用は結果を破棄しないため、
        // この余地が無いと応答を送り切れないまま接続が切れ得る。
        let batch = batch_timeout();
        let write = write_timeout();
        let server = ScaledBudgets::unscaled();
        let batch_request = server.server_request_phase(RequestBudgetKind::Batch);
        assert!(
            batch + write + server.transport_headroom() <= batch_request,
            "一括適用 {batch:?} と送信 {write:?} が要求フェーズ予算 {batch_request:?} に収まらない"
        );
    }

    #[test]
    fn an_expired_deadline_stops_the_batch_before_it_starts() {
        let adapter = FakeEditAdapter::new();
        let error = execute_edit(
            &adapter,
            &InstanceState::Ready,
            EditOperation::ApplyBatch,
            &batch_params(),
            RequestDeadline::Exceeded,
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(error.retryable);
        // 実行前に返す timeout だけが「変更は行われていない」と名乗れる。
        assert_eq!(error.details["change_applied"], json!("no"));
        assert_eq!(error.details["retry_requires"], json!("resend"));
        assert!(adapter.calls().is_empty(), "SDK を呼ばずに中止していません");
    }

    #[test]
    fn a_batch_within_the_deadline_reaches_the_adapter() {
        let adapter = FakeEditAdapter::new();
        let result = execute_edit(
            &adapter,
            &InstanceState::Ready,
            EditOperation::ApplyBatch,
            &batch_params(),
            RequestDeadline::Within(Instant::now() + Duration::from_secs(1)),
        )
        .expect("期限内の一括適用が拒否されました");

        assert_eq!(adapter.calls(), vec!["apply_batch"]);
        assert_eq!(result["project_epoch"], json!(EPOCH));
        assert_eq!(result["results"], json!([]));
    }

    #[test]
    fn a_batch_result_is_never_discarded_after_its_deadline() {
        // 一括適用が期限を使い切っても結果は捨てない。捨てると、1 要求ぶんの
        // 変更がまとめて要求元からは無応答として観測される。
        let now = Instant::now();
        assert_eq!(retained_send_deadline(now), now + write_timeout());

        // 読み取りは同じ状況で結果を捨てる。捨ててよい結果と、捨ててはいけない
        // 結果の差がここに出る。
        assert_eq!(
            decide_send(
                now,
                NOW_UNIX_MS,
                RequestDeadline::Within(now - Duration::from_millis(1)),
                None,
            ),
            SendDecision::Discard
        );
    }

    #[test]
    fn invalid_batch_params_are_rejected_before_the_lifecycle_state_is_checked() {
        // 起動処理中でも、要求内容の誤りは要求の誤りとして返す。
        let adapter = FakeEditAdapter::new();
        let error = execute_edit(
            &adapter,
            &InstanceState::Starting,
            EditOperation::ApplyBatch,
            &json!({ "operations": [] }),
            RequestDeadline::Within(Instant::now() + Duration::from_secs(1)),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(adapter.calls().is_empty());
    }

    #[test]
    fn batch_wide_rules_are_checked_before_the_batch_reaches_the_adapter() {
        // 件数・シーンの揃い・同じ状態を書き換える重複は、一括適用で初めて
        // 生じる要求内容の誤りである。実行口へ渡す前にここで落とす。
        let mut other_scene = fake_summary().selector;
        other_scene.scene_id = SCENE_ID + 1;
        let duplicate = json!({
            "type": "move_object",
            "selector": fake_summary().selector,
            "destination": { "layer": 1, "frame": 300 },
        });
        let cases = [
            ("件数 0", json!({ "operations": [] })),
            (
                "シーンの不揃い",
                json!({
                    "operations": [
                        duplicate,
                        {
                            "type": "move_object",
                            "selector": other_scene,
                            "destination": { "layer": 1, "frame": 400 },
                        },
                    ],
                }),
            ),
            (
                "同じ状態の重複",
                json!({ "operations": [duplicate, duplicate] }),
            ),
        ];

        // 一括適用で初めて生じる規則も、状態にも期限にも先んじて判定する。
        for (state, deadline, order) in misordering_cases() {
            for (label, params) in &cases {
                let adapter = FakeEditAdapter::new();
                let error = execute_edit(
                    &adapter,
                    &state,
                    EditOperation::ApplyBatch,
                    params,
                    deadline,
                )
                .unwrap_err();

                assert_eq!(
                    error.code,
                    ErrorCode::InvalidArgument,
                    "{order} の {label} が要求内容の誤りとして返りませんでした"
                );
                assert!(
                    adapter.calls().is_empty(),
                    "{order} の {label} が実行口へ届きました"
                );
            }
        }
    }

    #[test]
    fn batch_validation_failures_name_the_operation_that_failed() {
        // 100 件までを 1 要求で運ぶ operation に対し、位置の分からない
        // invalid_argument は訂正の手掛かりとして足りない。**要求元がこの層へ
        // 届く前に同じ検証を通っているとは限らない。** 検証を備えた口を
        // 経由しない要求でも、位置は同じ形で返る。
        let mut other_scene = fake_summary().selector;
        other_scene.scene_id = SCENE_ID + 1;
        let move_op = json!({
            "type": "move_object",
            "selector": fake_summary().selector,
            "destination": { "layer": 1, "frame": 300 },
        });
        let located = [
            (
                "シーンの不揃い",
                1,
                json!({
                    "operations": [
                        move_op,
                        {
                            "type": "move_object",
                            "selector": other_scene,
                            "destination": { "layer": 1, "frame": 400 },
                        },
                    ],
                }),
            ),
            (
                "同じ状態の重複",
                1,
                json!({ "operations": [move_op, move_op] }),
            ),
            (
                "sub-operation の内容",
                2,
                json!({
                    "operations": [
                        move_op,
                        {
                            "type": "move_object",
                            "selector": fake_summary().selector,
                            "destination": { "layer": 1, "frame": 500 },
                        },
                        {
                            "type": "set_object_item",
                            "selector": fake_effect_selector(),
                            "item": "ファイル",
                            "value": { "type": "file", "path": r"..\movie.mp4" },
                        },
                    ],
                }),
            ),
        ];

        for (label, index, params) in located {
            let adapter = FakeEditAdapter::new();
            let error = execute_edit(
                &adapter,
                &InstanceState::Ready,
                EditOperation::ApplyBatch,
                &params,
                within(),
            )
            .unwrap_err();

            assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
            assert_eq!(
                error.details["failed_index"],
                json!(index),
                "{label} が落ちた sub-operation の位置を運びませんでした"
            );
            assert!(adapter.calls().is_empty(), "{label}");
        }
    }

    #[test]
    fn a_batch_failure_without_a_position_does_not_name_one() {
        // 要求全体の誤りは特定の sub-operation に帰せられない。位置を添えると、
        // 要求元は 0 件目を直せば通ると読んでしまう。
        let too_many: Vec<Value> = (0..=aviutl2_mcp_core::MAX_BATCH_OPERATIONS)
            .map(|frame| {
                json!({
                    "type": "move_object",
                    "selector": fake_summary().selector,
                    "destination": { "layer": 1, "frame": frame },
                })
            })
            .collect();
        for (label, params) in [
            ("件数 0", json!({ "operations": [] })),
            ("件数超過", json!({ "operations": too_many })),
        ] {
            let adapter = FakeEditAdapter::new();
            let error = execute_edit(
                &adapter,
                &InstanceState::Ready,
                EditOperation::ApplyBatch,
                &params,
                within(),
            )
            .unwrap_err();

            assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
            assert_eq!(
                error.details.get("failed_index"),
                None,
                "{label} が位置を持たない失敗に位置を添えました: {:?}",
                error.details
            );
            assert!(adapter.calls().is_empty(), "{label}");
        }
    }

    #[test]
    fn batch_validation_failures_only_use_allowed_details_keys() {
        // 検証の失敗が返す補助情報も、実行の失敗と同じ許可キー一覧に従う。
        // 一覧に無いキーが出れば、要求元は解釈できない値を受け取る。
        const ALLOWED: &[&str] = &["failed_index", "reason"];

        let mut other_scene = fake_summary().selector;
        other_scene.scene_id = SCENE_ID + 1;
        let cases = [
            json!({ "operations": [] }),
            json!({
                "operations": [
                    {
                        "type": "move_object",
                        "selector": fake_summary().selector,
                        "destination": { "layer": 1, "frame": 300 },
                    },
                    {
                        "type": "move_object",
                        "selector": other_scene,
                        "destination": { "layer": 1, "frame": 400 },
                    },
                ],
            }),
            json!({
                "operations": [{
                    "type": "set_object_item",
                    "selector": fake_effect_selector(),
                    "item": "ファイル",
                    "value": { "type": "file", "path": r"\\.\pipe\aviutl2" },
                }],
            }),
        ];

        for params in cases {
            let adapter = FakeEditAdapter::new();
            let error = execute_edit(
                &adapter,
                &InstanceState::Ready,
                EditOperation::ApplyBatch,
                &params,
                within(),
            )
            .unwrap_err();

            for key in error.details.as_object().expect("補助情報は object").keys() {
                assert!(
                    ALLOWED.contains(&key.as_str()),
                    "検証の失敗の補助情報に未許可のキー {key} が含まれています"
                );
            }

            // 位置は整数だけであり、対象の内容を運ばない。設定値・alias・
            // パスそのものは説明にも補助情報にも現れない。
            let document = format!("{} {}", error.message, error.details);
            for forbidden in [r"\\.", "movie.mp4", "pipe", "[1:100]", "0x"] {
                assert!(
                    !document.contains(forbidden),
                    "{forbidden} が応答に含まれます: {document}"
                );
            }
        }
    }

    /// 期限判定の基準時刻。読み取り側のテストと同じ値を用いる。
    const NOW_UNIX_MS: i64 = 1_785_144_000_000;
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use aviutl2_mcp_core::{RenderFrameResult, RequestBudgetKind};
    use serde_json::json;
    use std::sync::Mutex;

    const EPOCH: &str = "9d0a5f4e-2f47-4a13-9a5e-1e2f3a4b5c6d";
    const SCENE_ID: i32 = 0;
    const FRAME: u32 = 42;

    /// 引き渡し用ファイルの識別子。小文字 16 進 32 文字。
    const HANDOFF_TOKEN: &str = "0123456789abcdef0123456789abcdef";

    /// 成果物の置き場。応答にも補助情報にも現れてはならない値の代表。
    const ARTIFACT_PATH: &str = r"C:\Users\example\artifacts\frame.png";

    /// レンダリングの実行口の代わりに定型データを返す実装。
    ///
    /// 呼び出しを記録するため、受付判定や params の検証で弾かれた要求が実行口へ
    /// 進んでいないことを確かめられる。
    struct FakeRenderAdapter {
        calls: Mutex<Vec<&'static str>>,
        discarded: Mutex<Vec<String>>,
        /// 最初の呼び出しで返す失敗。
        failure: Mutex<Option<RenderError>>,
    }

    impl FakeRenderAdapter {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                discarded: Mutex::new(Vec::new()),
                failure: Mutex::new(None),
            }
        }

        /// 最初のレンダリングが指定の失敗を返す実行口を作る。
        fn failing(error: RenderError) -> Self {
            let adapter = Self::new();
            *adapter.failure.lock().unwrap() = Some(error);
            adapter
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        fn discarded(&self) -> Vec<String> {
            self.discarded.lock().unwrap().clone()
        }
    }

    impl RenderAdapter for FakeRenderAdapter {
        fn render_frame(
            &self,
            params: &RenderFrameParams,
        ) -> Result<RenderFrameResult, RenderError> {
            self.calls.lock().unwrap().push("render_frame");
            if let Some(error) = self.failure.lock().unwrap().take() {
                return Err(error);
            }
            Ok(RenderFrameResult {
                project_epoch: EPOCH.to_string(),
                project_revision: 3,
                scene_id: params.expected_scene_id,
                frame: params.frame,
                width: 320,
                height: 180,
                media_type: crate::render::ARTIFACT_MEDIA_TYPE.to_string(),
                byte_length: 4,
                sha256: format!("sha256:{}", "0".repeat(64)),
                handoff_token: HANDOFF_TOKEN.to_string(),
            })
        }

        fn discard_artifact(&self, handoff_token: &str) {
            self.discarded
                .lock()
                .unwrap()
                .push(handoff_token.to_string());
        }
    }

    /// 受け付けられる要求の params。
    fn render_params() -> Value {
        json!({ "expected_scene_id": SCENE_ID, "frame": FRAME })
    }

    /// 期限内の判定。
    fn within() -> RequestDeadline {
        RequestDeadline::Within(Instant::now() + Duration::from_secs(1))
    }

    #[test]
    fn every_render_operation_is_routed_from_its_name() {
        for operation in RenderOperation::ALL {
            assert_eq!(
                classify_operation(operation.as_str()).unwrap(),
                Operation::Render(operation),
                "{} がレンダリングへ振り分けられていません",
                operation.as_str()
            );
        }
    }

    #[test]
    fn a_render_within_the_deadline_reaches_the_adapter() {
        let adapter = FakeRenderAdapter::new();
        let result = execute_render(
            &adapter,
            &InstanceState::Ready,
            RenderOperation::RenderFrame,
            &render_params(),
            within(),
        )
        .expect("期限内のレンダリングが拒否されました");

        assert_eq!(adapter.calls(), vec!["render_frame"]);
        assert_eq!(result.scene_id, SCENE_ID);
        assert_eq!(result.frame, FRAME);
        assert_eq!(result.handoff_token, HANDOFF_TOKEN);
    }

    #[test]
    fn render_params_are_decoded_before_the_state_and_the_deadline() {
        // 要求内容の誤りはライフサイクル状態にも期限にも依存しない。受付判定を
        // 先に通すと、解消しない誤りが再試行を促す host_busy として返る。期限
        // 判定を先に通すと、同じ誤りが再試行可能な timeout に化ける。**どちらの
        // 順序も塞ぐ。** 片方だけを見ていると、もう一方へ入れ替える変更が
        // 素通りする。
        for (state, deadline, order) in [
            (InstanceState::Starting, within(), "起動処理中"),
            (InstanceState::Ready, RequestDeadline::Exceeded, "期限超過"),
        ] {
            for params in [
                json!({ "expected_scene_id": SCENE_ID }),
                json!({ "expected_scene_id": SCENE_ID, "frame": FRAME, "future": 1 }),
                json!({ "expected_scene_id": SCENE_ID, "frame": -1 }),
                json!({ "expected_scene_id": SCENE_ID, "frame": u32::MAX }),
                json!({ "frame": FRAME }),
            ] {
                let adapter = FakeRenderAdapter::new();
                let error = execute_render(
                    &adapter,
                    &state,
                    RenderOperation::RenderFrame,
                    &params,
                    deadline,
                )
                .unwrap_err();

                assert_eq!(
                    error.code,
                    ErrorCode::InvalidArgument,
                    "{order} の {params} が要求内容の誤りとして返りませんでした"
                );
                assert!(adapter.calls().is_empty(), "{order}: {params}");
            }
        }
    }

    #[test]
    fn a_starting_instance_rejects_a_well_formed_render() {
        let adapter = FakeRenderAdapter::new();
        let error = execute_render(
            &adapter,
            &InstanceState::Starting,
            RenderOperation::RenderFrame,
            &render_params(),
            within(),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::HostBusy);
        assert!(error.retryable);
        assert!(adapter.calls().is_empty());
    }

    #[test]
    fn an_expired_deadline_stops_the_render_before_it_starts() {
        let adapter = FakeRenderAdapter::new();
        let error = execute_render(
            &adapter,
            &InstanceState::Ready,
            RenderOperation::RenderFrame,
            &render_params(),
            RequestDeadline::Exceeded,
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(error.retryable);
        assert!(
            adapter.calls().is_empty(),
            "期限超過後にホストへタスクを投入しました"
        );
    }

    #[test]
    fn a_render_timeout_never_warns_about_an_applied_change() {
        // レンダリングはプロジェクトを変更しない。変更の有無を伝えるキーを
        // 添えると、要求元は編集と同じ警戒（読み直してから再送）を要すると
        // 誤解する。
        let adapter = FakeRenderAdapter::new();
        let render = execute_render(
            &adapter,
            &InstanceState::Ready,
            RenderOperation::RenderFrame,
            &render_params(),
            RequestDeadline::Exceeded,
        )
        .unwrap_err();
        assert_eq!(render.code, ErrorCode::Timeout);
        assert_eq!(render.details.get("change_applied"), None);
        assert_eq!(render.details.get("mutation_origin"), None);

        // 完了を待てなかった失敗にも付かない。代わりに落ちた段を伝える。
        let waited = render_error(RenderError::WaitTimeout);
        assert_eq!(waited.code, ErrorCode::Timeout);
        assert_eq!(waited.details.get("change_applied"), None);
        assert_eq!(waited.details["render_stage"], json!("wait"));

        // 編集の同じ経路には付く。変更が入ったか判別できる側だけが名乗る。
        let edit = edit_timeout_before_execution();
        assert_eq!(edit.code, ErrorCode::Timeout);
        assert_eq!(edit.details["change_applied"], json!("no"));
    }

    #[test]
    fn a_render_result_is_never_discarded_after_its_deadline() {
        // レンダリングの結果を捨てると引き渡し用ファイルが宙に浮く。受け取る
        // 側は識別子を得ていないため掃除できない。
        let now = Instant::now();
        assert_eq!(retained_send_deadline(now), now + write_timeout());

        // 読み取りは同じ状況で結果を捨てる。破棄経路をレンダリングへ
        // 再利用しないことが、この差として現れる。
        assert_eq!(
            decide_send(
                now,
                NOW_UNIX_MS,
                RequestDeadline::Within(now - Duration::from_millis(1)),
                None,
            ),
            SendDecision::Discard
        );
    }

    /// 実行口が返した成果物つきの結果。
    fn rendered(adapter: &FakeRenderAdapter) -> RenderFrameResult {
        execute_render(
            adapter,
            &InstanceState::Ready,
            RenderOperation::RenderFrame,
            &render_params(),
            within(),
        )
        .expect("期限内のレンダリングが拒否されました")
    }

    #[test]
    fn a_failed_send_returns_the_artifact_to_the_adapter() {
        let adapter = FakeRenderAdapter::new();

        // 送信できた成果物は受け取る側が所有する。消してはならない。
        deliver_render_response(&adapter, Ok(rendered(&adapter)), |outcome| {
            // 応答へ載るのは実行口が返した結果そのものである。
            assert_eq!(outcome.unwrap()["handoff_token"], json!(HANDOFF_TOKEN));
            Ok(())
        })
        .expect("送信できた応答が失敗として返りました");
        assert!(
            adapter.discarded().is_empty(),
            "送信できた成果物を消しました"
        );

        // 送信に失敗すれば受け取る側は識別子を持たない。ここで消す。
        deliver_render_response(&adapter, Ok(rendered(&adapter)), |_| {
            anyhow::bail!("応答の送信に失敗しました")
        })
        .expect_err("送信の失敗が握り潰されました");
        assert_eq!(adapter.discarded(), vec![HANDOFF_TOKEN.to_string()]);
    }

    #[test]
    fn a_failed_send_of_a_failure_response_has_no_artifact_to_return() {
        // 失敗した要求は成果物を作っていない。消すものが無いのに実行口を
        // 呼ぶと、他の要求の成果物を消しかねない。
        let adapter = FakeRenderAdapter::new();
        let failure = render_error(RenderError::WaitTimeout);
        deliver_render_response(&adapter, Err(failure.clone()), |outcome| {
            assert_eq!(outcome.unwrap_err(), failure);
            anyhow::bail!("応答の送信に失敗しました")
        })
        .expect_err("送信の失敗が握り潰されました");
        assert!(adapter.discarded().is_empty());
    }

    #[test]
    fn only_the_render_response_leaves_something_to_clean_up() {
        // レンダリングの結果は引き渡し用ファイルを 1 つ残す。だから送信に
        // 失敗したときの後始末が要る。
        let adapter = FakeRenderAdapter::new();
        assert!(!rendered(&adapter).handoff_token.is_empty());

        // 一括適用の結果は掃除すべきものを 1 つも残さない。変更は既に取り消し
        // 単位へ登録されており、応答を送れなくても差し戻せるものは無い。
        let outcome = serde_json::to_value(aviutl2_mcp_core::BatchOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 1,
            results: Vec::new(),
        })
        .unwrap();
        assert_eq!(outcome.get("handoff_token"), None);
        assert!(!outcome.to_string().contains(HANDOFF_TOKEN));
    }

    /// レンダリングの失敗の分類と、その代表値を 1 つの宣言から作る。
    ///
    /// **`RenderError` を直接並べない。** 失敗は複製できないため一覧は生成
    /// 関数の列になり、生成関数の列は網羅性を型で縛れない。分類の列挙を挟むと、
    /// [`RenderFailure::error`] の網羅 `match` が分類の追加でコンパイルを止める。
    ///
    /// **一覧と生成を別々に書かない。** 分けると、一覧から 1 件落ちても生成側は
    /// 動いたままに見える。同じ `RenderError` variant を 2 つの分類が指す場合、
    /// 落ちた 1 件は variant の網羅を見るテストからも隠れてしまう。
    macro_rules! render_failures {
        ($($variant:ident => $error:expr),+ $(,)?) => {
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            enum RenderFailure {
                $($variant),+
            }

            impl RenderFailure {
                /// 全分類。宣言と同じ並びで、落とすことも増やすこともできない。
                const ALL: &'static [RenderFailure] = &[$(RenderFailure::$variant),+];

                /// 対応する失敗の代表値を作る。
                fn error(self) -> RenderError {
                    match self {
                        $(RenderFailure::$variant => $error),+
                    }
                }
            }
        };
    }

    render_failures! {
        Read => RenderError::Read(crate::read::ReadError::NotReady),
        ReadEditBlocked => RenderError::Read(crate::read::ReadError::EditBlocked {
            state: crate::read::EditState::Save,
        }),
        SceneMismatch => RenderError::SceneMismatch {
            expected: SCENE_ID,
            current: SCENE_ID + 1,
        },
        FrameOutOfRange => RenderError::FrameOutOfRange,
        FrameTooLarge => RenderError::FrameTooLarge,
        WaitTimeout => RenderError::WaitTimeout,
        ShuttingDown => RenderError::ShuttingDown,
        TooManyAbandoned => RenderError::TooManyAbandoned,
        InvalidBuffer => RenderError::InvalidBuffer {
            rule: crate::render::BufferRule::PitchTooSmall,
        },
        ArtifactEncode => RenderError::Artifact {
            stage: crate::render::ArtifactStage::Encode,
        },
        ArtifactWrite => RenderError::Artifact {
            stage: crate::render::ArtifactStage::Write,
        },
        Sdk => RenderError::Sdk {
            operation: "rendering_scene_video",
        },
        Panicked => RenderError::Panicked,
    }

    #[test]
    fn every_render_error_variant_has_a_failure_case() {
        // 分類の網羅 match は `RenderError` の variant 追加を止めない。
        // variant を 1 つずつ名指しし、名前の集合が一致することを主張する。
        // ここが落ちたら、足した variant を [`RenderFailure`] へも足す。
        fn variant_name(error: &RenderError) -> &'static str {
            match error {
                RenderError::Read(_) => "Read",
                RenderError::SceneMismatch { .. } => "SceneMismatch",
                RenderError::FrameOutOfRange => "FrameOutOfRange",
                RenderError::FrameTooLarge => "FrameTooLarge",
                RenderError::WaitTimeout => "WaitTimeout",
                RenderError::ShuttingDown => "ShuttingDown",
                RenderError::TooManyAbandoned => "TooManyAbandoned",
                RenderError::InvalidBuffer { .. } => "InvalidBuffer",
                RenderError::Artifact { .. } => "Artifact",
                RenderError::Sdk { .. } => "Sdk",
                RenderError::Panicked => "Panicked",
            }
        }

        const VARIANTS: &[&str] = &[
            "Read",
            "SceneMismatch",
            "FrameOutOfRange",
            "FrameTooLarge",
            "WaitTimeout",
            "ShuttingDown",
            "TooManyAbandoned",
            "InvalidBuffer",
            "Artifact",
            "Sdk",
            "Panicked",
        ];

        let mut covered: Vec<&str> = RenderFailure::ALL
            .iter()
            .map(|failure| variant_name(&failure.error()))
            .collect();
        covered.sort_unstable();
        covered.dedup();

        let mut expected = VARIANTS.to_vec();
        expected.sort_unstable();
        assert_eq!(covered, expected, "代表値が全 variant を覆っていません");

        // variant の網羅だけでは足りない。`Read` のように内側の値で振る舞いが
        // 変わる variant は代表値を複数持つため、そのうち 1 件を落としても
        // 上の主張は通ってしまう。**観測結果の集合そのものを固定する。**
        let observed: Vec<(String, String, String)> = RenderFailure::ALL
            .iter()
            .map(|failure| {
                let error = failure.error();
                (
                    error.error_code().to_string(),
                    error.to_string(),
                    error.details().to_string(),
                )
            })
            .collect();

        let mut unique = observed.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            observed.len(),
            "同じ観測結果を返す代表値が重複しています。増やしても網羅は増えません"
        );
        assert_eq!(
            observed.len(),
            13,
            "代表値の件数が変わりました。減らすなら、失われた観測結果が \
             他の代表値で覆われていることを確かめてください"
        );
    }

    #[test]
    fn render_failures_keep_their_code_and_details() {
        for &failure in RenderFailure::ALL {
            let expected = failure.error();
            let adapter = FakeRenderAdapter::failing(failure.error());

            let error = execute_render(
                &adapter,
                &InstanceState::Ready,
                RenderOperation::RenderFrame,
                &render_params(),
                within(),
            )
            .unwrap_err();

            assert_eq!(error.code, expected.error_code(), "{expected}");
            assert_eq!(error.retryable, expected.retryable(), "{expected}");
            assert_eq!(error.message, expected.to_string(), "{expected}");
            assert_eq!(error.details, expected.details(), "{expected}");
        }
    }

    #[test]
    fn render_failures_never_produce_cancelled() {
        // 受信から実行までの窓も保留キューも無いため、取り消しは生成しない。
        for &failure in RenderFailure::ALL {
            let adapter = FakeRenderAdapter::failing(failure.error());
            let error = execute_render(
                &adapter,
                &InstanceState::Ready,
                RenderOperation::RenderFrame,
                &render_params(),
                within(),
            )
            .unwrap_err();
            assert_ne!(error.code, ErrorCode::Cancelled, "{}", error.message);
        }

        for deadline in [RequestDeadline::Exceeded, within()] {
            for state in [
                InstanceState::Starting,
                InstanceState::Draining,
                InstanceState::Gone,
            ] {
                let adapter = FakeRenderAdapter::new();
                let error = execute_render(
                    &adapter,
                    &state,
                    RenderOperation::RenderFrame,
                    &render_params(),
                    deadline,
                )
                .unwrap_err();
                assert_ne!(error.code, ErrorCode::Cancelled, "{state}");
            }
        }
    }

    #[test]
    fn render_failures_only_use_allowed_details_keys() {
        // 補助情報のキーはここで列挙したものに限る。新しいキーを足す際は、
        // 引き渡し用の識別子・パス・画像でないことを確かめた上で追加する。
        const ALLOWED: &[&str] = &[
            "retry_after_ms",
            "edit_state",
            "expected_scene_id",
            "current_scene_id",
            "mismatch",
            "reason",
            "render_stage",
            "sdk_operation",
            "retry_requires",
        ];
        for &failure in RenderFailure::ALL {
            let adapter = FakeRenderAdapter::failing(failure.error());
            let error = execute_render(
                &adapter,
                &InstanceState::Ready,
                RenderOperation::RenderFrame,
                &render_params(),
                within(),
            )
            .unwrap_err();

            for key in error.details.as_object().expect("補助情報は object").keys() {
                assert!(
                    ALLOWED.contains(&key.as_str()),
                    "{} の補助情報に未許可のキー {key} が含まれています",
                    error.message
                );
            }
        }
    }

    #[test]
    fn neither_the_handoff_token_nor_a_path_reaches_a_failure_response() {
        // 引き渡し用の識別子を渡せば、それだけで成果物の在り処が分かる。画像
        // には利用者のプロジェクトの内容が写る。どちらも失敗の応答へ出さない。
        let mut documents = Vec::new();
        for &failure in RenderFailure::ALL {
            let adapter = FakeRenderAdapter::failing(failure.error());
            let error = execute_render(
                &adapter,
                &InstanceState::Ready,
                RenderOperation::RenderFrame,
                &render_params(),
                within(),
            )
            .unwrap_err();
            documents.push(serde_json::to_string(&error).unwrap());
        }
        documents.push(
            serde_json::to_string(
                &execute_render(
                    &FakeRenderAdapter::new(),
                    &InstanceState::Ready,
                    RenderOperation::RenderFrame,
                    &render_params(),
                    RequestDeadline::Exceeded,
                )
                .unwrap_err(),
            )
            .unwrap(),
        );

        for document in documents {
            let lowered = document.to_lowercase();
            for forbidden in [HANDOFF_TOKEN, ARTIFACT_PATH, ".png", r"c:\", "token", "0x"] {
                assert!(
                    !lowered.contains(&forbidden.to_lowercase()),
                    "{forbidden} が応答に含まれます: {document}"
                );
            }
        }
    }

    #[test]
    fn a_render_is_given_its_own_execution_budget() {
        assert_eq!(
            execution_timeout(Operation::Render(RenderOperation::RenderFrame)),
            render_timeout()
        );
        // 最も短い予算へ落ちると、投入した瞬間に予算が尽きる。
        assert_ne!(render_timeout(), read_timeout());
        assert_ne!(render_timeout(), edit_timeout());

        // 実行の上限は、実行口が持つ 2 つの段の取り分をどちらも覆う。片方だけを
        // 数えると、もう一方の段へ入った時点で既に上限を超えている。
        assert!(render_timeout() > budgets().plugin_render_wait());
        assert!(render_timeout() > budgets().plugin_render_artifact());

        // レンダリングが実行の上限まで走っても、応答送信と、要求元が応答を
        // 受けてから行う成果物の引き取りの持ち時間が要求フェーズ予算の内側に
        // 残る。**引き取りの段を数え忘れると、どの層の期限にも捕まらないまま
        // 予算を超えてから成功する経路ができる。**
        let render = render_timeout();
        let write = write_timeout();
        let server = ScaledBudgets::unscaled();
        let ingest = server.server_artifact_ingest();
        let render_request = server.server_request_phase(RequestBudgetKind::Render);
        assert!(
            render + write + server.transport_headroom() + ingest <= render_request,
            "レンダリング {render:?} と送信 {write:?} と引き取り {ingest:?} が要求フェーズ予算 {render_request:?} に収まらない"
        );
    }

    /// 期限判定の基準時刻。読み取り側のテストと同じ値を用いる。
    const NOW_UNIX_MS: i64 = 1_785_144_000_000;
}
