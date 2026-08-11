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

mod decode;
mod dispatch;

use crate::edit::{EditAdapter, EditError};
use crate::lifecycle::Lifecycle;
use crate::pipe::PipeStream;
use crate::read::{ReadAdapter, ReadError};
use crate::render::{RenderAdapter, RenderError};
use anyhow::{Context, Result};
use aviutl2_mcp_core::{
    BatchInputError, ClientAuth, ClientHello, DescribeEffectsInputError, EditInputError,
    EditOperation, EffectItemValuesInputError, ErrorCode, ErrorObject, InstanceId, InstanceState,
    KnownOperation, LimitOutOfRange, Nonce, ObjectFilterError, PongProject, PongResult,
    ProtocolVersion, ReadOperation, RenderFrameResult, RenderInputError, RenderOperation,
    RequestEnvelope, RequestId, ResponseEnvelope, ResponseKind, ResponseResult, ScaledBudgets,
    SnapshotRevisionMismatch, TextSyntaxError, compute_client_mac, compute_server_mac,
    deserialize_json, verify_mac,
};
use chrono::Utc;
use decode::{decode_edit_request, decode_render_request, decode_request};
use dispatch::{dispatch_edit, dispatch_read, dispatch_render, to_result};
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
    PongResult::new(
        instance_id,
        state,
        PongProject {
            epoch: status.epoch,
            revision: status.revision,
            modified: status.modified,
        },
    )
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
    }
}

/// 一時的に要求を受け付けられないことを、再試行の案内つきで返す。
fn host_busy(message: &str) -> ErrorObject {
    error_object(ErrorCode::HostBusy, message)
        .with_details(json!({ "retry_after_ms": HOST_BUSY_RETRY_AFTER_MS }))
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

/// effect の中身の要求内容の失敗を応答用のエラーへ変換する。
///
/// **どの規則で落ちたかを機械可読な形で添える。** 件数・重複・名前の構文は
/// 要求元が取れる行動が異なり、名前が無ければ説明の文面を解析するほかない。
/// 検証に失敗した effect 名そのものは説明にも補助情報にも現れない。
pub(crate) fn describe_effects_error(error: DescribeEffectsInputError) -> ErrorObject {
    with_details(
        error_object(ErrorCode::InvalidArgument, error.to_string()),
        input_error_details(Some(error.reason()), None),
    )
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
mod tests;

#[cfg(test)]
mod edit_tests;

#[cfg(test)]
mod render_tests;
