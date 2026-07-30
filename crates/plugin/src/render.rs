//! 現在シーンの 1 フレームを描き、成果物をプロセスの外へ渡す層。
//!
//! # 他の層と決定的に違うこと
//!
//! 読み取りも編集も、SDK の呼び出しが戻った時点で結果が手に入る。レンダリングは
//! そうではない。要求はタスクの投入だけで完了し、**完了はホストのレンダリング用
//! スレッドから我々のコードを呼ぶ形で届く**。この経路には次の性質がある。
//!
//! - 投入が成功しても、完了が届く保証は無い。
//! - 投入したタスクを取り消す手段が無い。
//! - 完了が届く時刻に上限が無い。
//!
//! したがって「待てば必ず終わる」前提では組めない。要求元は期限付きで待ち、
//! 超えたら受け皿を放棄する（[`slot`]）。放棄しても、ホストが抱えるタスクは
//! 生き続ける。
//!
//! # 区間の外で行う
//!
//! レンダリングの投入と待機は、読み取り区間・編集区間の内側では行わない。
//! 完了待ちは参照ロック・編集ロックの下で呼ぶとデッドロックし得るためであり、
//! 必要な編集情報は区間の外で取る。[`host::RenderHost`] は区間へ入る口を
//! 持たない。
//!
//! # 構成
//!
//! | モジュール | 担当 |
//! |---|---|
//! | [`error`] | 失敗の分類と、応答へ載せる安全な補助情報 |
//! | [`buffer`] | ホストが渡す pixel buffer の検証と詰め直し |
//! | [`slot`] | 完了コールバックと要求元が共有する受け皿、投入したタスクの在庫 |
//! | [`handoff`] | 成果物の符号化・原子的な書き出し・掃除 |
//! | [`host`] | SDK 境界。完了コールバックのクロージャはここで組む |
//! | [`adapter`] | 受付判定・シーン照合・検証・待機・成果物の書き出し |
//! | [`sdk`] | SDK 境界の実装 |

pub mod adapter;
pub mod buffer;
pub mod error;
pub mod handoff;
pub mod host;
pub mod sdk;
pub mod slot;

use crate::project::ProjectState;
use anyhow::Result;
use aviutl2_mcp_core::{InstanceId, RenderFrameParams, RenderFrameResult};
use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

pub use adapter::HostRenderAdapter;
pub use buffer::{BYTES_PER_PIXEL, ExtractedFrame, FrameLayout, MAX_RENDER_FRAME_BYTES};
pub use error::{ArtifactStage, BufferRule, RenderError, RenderStage};
pub use handoff::{ARTIFACT_MEDIA_TYPE, HANDOFF_TTL, HandoffArtifact, HandoffDir, HandoffToken};
pub use slot::{
    ABANDONED_ENTRY_TTL, MAX_ABANDONED_RENDERS, RENDER_WAIT_TICK, RenderInventory, RenderSlot,
    RenderedFrame, SlotWait, StopRequest, deliver_frame, deliver_frame_guarded, guard_callback,
};

/// 終了手順が投入済みタスクの完了を待つ上限。
///
/// 完了コールバックの待ち時間よりずっと短い。**期限まで待って放棄された
/// タスクが、この時間で完了する見込みは高くない。** それでも有限にするのは、
/// ホストの終了を無期限に止める方が確実に有害だからである。利用者はプロセスを
/// 強制終了するしかなくなり、その方がデータを失う。
pub const RENDER_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// レンダリング operation の実行口。
///
/// 1 度の呼び出しで完結し、SDK の型・ハンドル・区間には触れない。編集ではない
/// ため編集の実行口へは同居させない。
pub trait RenderAdapter: Send + Sync {
    /// 現在シーンの指定フレームを描き、成果物を引き渡し用ファイルへ書く。
    fn render_frame(&self, params: &RenderFrameParams) -> Result<RenderFrameResult, RenderError>;

    /// 応答を送れなかった成果物を消す。
    ///
    /// **成功した結果を破棄しないこと**と対になる。レンダリングは副作用を
    /// 持たないため結果を捨ててもよいが、捨てると引き渡し用ファイルが宙に浮く。
    /// 受け取る側は識別子を得ていないため掃除できない。送信できたなら受け取る
    /// 側が即座に所有して片付けられるので、破棄せず送る。送信そのものに失敗した
    /// ときだけ、ここで消す。
    fn discard_artifact(&self, handoff_token: &str);
}

/// 終了手順から見たレンダリングの在庫。
///
/// 要求の実行口と分けているのは、終了手順が要求を発行しないためである。
/// 同じ型が両方を実装するが、終了手順はこちらしか見ない。
pub trait RenderDrain: Send + Sync {
    /// 投入済みで完了していないタスクと、放棄済みのタスクの合計。
    ///
    /// **時間による減衰を適用しない。** 忘れたタスクも、生きているなら
    /// アンロード後に飛んでくる。
    fn outstanding(&self) -> usize;

    /// 投入済みタスクが全て完了するまで待つ。期限を取らない。
    fn wait_all_tasks(&self);

    /// 自インスタンスの引き渡し用ファイルをまとめて消す。
    fn discard_artifacts(&self);
}

/// SDK を実際に呼び出す render adapter を作る。
///
/// 編集ハンドルが未初期化・未準備の間も生成でき、その状態のレンダリングは
/// SDK を呼ばずに拒否される。
pub fn sdk_render_adapter(
    project_state: Arc<ProjectState>,
    instance_id: &InstanceId,
    stop: Arc<dyn StopRequest>,
) -> Result<Arc<HostRenderAdapter<sdk::SdkRenderHost>>> {
    let handoff = HandoffDir::new(instance_id)?;
    Ok(Arc::new(HostRenderAdapter::new(
        sdk::SdkRenderHost::new(),
        project_state,
        handoff,
        stop,
    )))
}

/// アンロード前に、ホストが抱えるレンダリングタスクを空にしようとする。
///
/// 順序が要である。**接続受理を止めた後に呼ぶ。** 止める前に数えると、その後に
/// 投入されたタスクを取りこぼす。
///
/// 数えるのは「放棄されたか」ではなく「投入して完了していないか」である。実行中の
/// タスクは放棄済みではないため、放棄済みだけを数えると 0 件と判定して待機を
/// 飛ばし、その直後のアンロードへコールバックが飛び込む。
///
/// 在庫が空なら待たずに戻る。**費用も危険も無い経路を既定にする。**
///
/// # 期限内に戻らなかった場合
///
/// スレッドを切り離して進む。**この場合、アンロード後に届くコールバックは
/// 防げていない。** コールバックの入口は我々のコードであり、アンロード後に
/// 呼ばれればアンマップされた領域へ飛ぶ。これを止める手段は、我々が使っている
/// SDK ラッパーの側に無い。ここで進むのは、止まる方が確実に有害だからであって、
/// 危険が無くなるからではない。
///
/// 待機は専用スレッドで行う。**このスレッドは plugin の singleton へ触れて
/// はならない。** ホストのアンロードは singleton の書き込みロックを保持したまま
/// plugin を解体し、その解体がこの待機を join する。
pub fn drain_render_tasks<D>(drain: &Arc<D>, timeout: Duration)
where
    D: RenderDrain + 'static,
{
    let outstanding = drain.outstanding();
    if outstanding == 0 {
        return;
    }

    // 送信は行わない。スレッド終了時に送信端が解放され、受信側が切断を
    // 得ることで終了を検知する。
    let (tx, rx) = channel::<()>();
    let waiting = Arc::clone(drain);
    let join_handle = std::thread::spawn(move || {
        let _finished = tx;
        waiting.wait_all_tasks();
    });

    if matches!(
        rx.recv_timeout(timeout),
        Err(RecvTimeoutError::Disconnected)
    ) {
        if join_handle.join().is_err() {
            tracing::error!("レンダリングの完了待ちが panic で終了しました");
        }
        return;
    }
    tracing::warn!(
        outstanding,
        "レンダリングの完了待ちが {}ms で期限を超えたため切り離しました",
        timeout.as_millis()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    /// 在庫の件数と待ちの振る舞いを差し替えられる終了口。
    struct FakeDrain {
        outstanding: usize,
        /// 全タスク待ちが戻るまでの時間。
        waits_for: Duration,
        waited: AtomicUsize,
        calls: Mutex<Vec<&'static str>>,
    }

    impl FakeDrain {
        fn new(outstanding: usize, waits_for: Duration) -> Arc<Self> {
            Arc::new(Self {
                outstanding,
                waits_for,
                waited: AtomicUsize::new(0),
                calls: Mutex::new(Vec::new()),
            })
        }

        fn waited(&self) -> usize {
            self.waited.load(Ordering::Acquire)
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl RenderDrain for FakeDrain {
        fn outstanding(&self) -> usize {
            self.calls.lock().unwrap().push("outstanding");
            self.outstanding
        }

        fn wait_all_tasks(&self) {
            self.calls.lock().unwrap().push("wait_all_tasks");
            self.waited.fetch_add(1, Ordering::AcqRel);
            std::thread::sleep(self.waits_for);
        }

        fn discard_artifacts(&self) {
            self.calls.lock().unwrap().push("discard_artifacts");
        }
    }

    #[test]
    fn an_empty_inventory_never_waits() {
        // 投入したタスクが全て完了していれば待つ必要が無い。費用も危険も無い
        // 経路を既定にする。
        let drain = FakeDrain::new(0, Duration::ZERO);
        drain_render_tasks(&drain, RENDER_DRAIN_TIMEOUT);
        assert_eq!(drain.waited(), 0);
        assert_eq!(drain.calls(), vec!["outstanding"]);
    }

    #[test]
    fn an_outstanding_task_is_waited_for() {
        // 実行中のタスクは放棄済みではない。放棄済みだけを数える実装はここで
        // 0 件と判定し、待たずにアンロードへ進む。
        let drain = FakeDrain::new(1, Duration::ZERO);
        drain_render_tasks(&drain, RENDER_DRAIN_TIMEOUT);
        assert_eq!(drain.waited(), 1);
    }

    #[test]
    fn a_wait_that_does_not_return_is_detached_within_the_timeout() {
        let drain = FakeDrain::new(1, Duration::from_secs(30));
        let timeout = Duration::from_millis(200);

        let started = Instant::now();
        drain_render_tasks(&drain, timeout);
        let elapsed = started.elapsed();

        assert_eq!(drain.waited(), 1);
        assert!(
            elapsed < Duration::from_secs(5),
            "戻らない待機を切り離せていません: {}ms",
            elapsed.as_millis()
        );
        assert!(
            elapsed >= timeout,
            "期限を待たずに切り離しました: {}ms",
            elapsed.as_millis()
        );
    }
}
