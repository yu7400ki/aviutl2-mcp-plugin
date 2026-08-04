//! レンダリング operation の手順。
//!
//! SDK 呼び出しは [`RenderHost`] へ委ね、ここでは受付可否の判定・シーンの照合・
//! フレームと大きさの検証・完了の待機・成果物の書き出しだけを行う。SDK の型は
//! 現れない。
//!
//! # 順序が意味を持つ箇所
//!
//! - **編集情報の値を、その値を上限として使う前に検証する。** 最終フレームは
//!   ホストから来る信頼できない値であり、先に範囲を疑わなければフレームの
//!   範囲判定が何も弾かなくなる。
//! - **シーンは投入前と完了後の 2 回照合する。** 投入から完了までの間に利用者が
//!   シーンを切り替えれば、返ってくるのは別のシーンの絵である。コールバックは
//!   フレームを返すがシーンを返さないため、絵そのものからは判別できない。
//! - **完了後の照合は符号化と書き出しの前に行う。** 捨てると決まっているものへ
//!   入出力の費用を払わない。

use crate::project::ProjectState;
use crate::read::host::HostEditInfo;
use crate::read::{EditState, ReadError};
use crate::render::buffer::BYTES_PER_PIXEL;
use crate::render::error::RenderError;
use crate::render::handoff::{ARTIFACT_MEDIA_TYPE, HandoffDir, HandoffToken};
use crate::render::host::RenderHost;
use crate::render::slot::{RenderInventory, RenderSlot, SlotWait, StopRequest};
use crate::render::{RenderAdapter, RenderDrain};
use aviutl2_mcp_core::{
    MAX_RENDER_FRAME_BYTES, PLUGIN_RENDER_WAIT_TIMEOUT, RenderFrameParams, RenderFrameResult,
};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// [`RenderHost`] の上にレンダリング operation を実装した adapter。
pub struct HostRenderAdapter<H> {
    host: H,
    project: Arc<ProjectState>,
    handoff: HandoffDir,
    inventory: Arc<RenderInventory>,
    stop: Arc<dyn StopRequest>,
    wait_timeout: Duration,
}

impl<H> HostRenderAdapter<H> {
    /// ホスト・プロジェクト状態・引き渡し用ディレクトリ・停止の観測口から
    /// adapter を作る。
    ///
    /// 観測する停止は、接続受理ループを止めるものと**同一のもの**でなければ
    /// ならない。要求処理は接続を受けるスレッド上で同期実行されるため、別の
    /// 合図を渡すと、完了待ちが停止手順を待たせたままになる。
    pub fn new(
        host: H,
        project: Arc<ProjectState>,
        handoff: HandoffDir,
        stop: Arc<dyn StopRequest>,
    ) -> Self {
        Self {
            host,
            project,
            handoff,
            inventory: RenderInventory::new(),
            stop,
            wait_timeout: PLUGIN_RENDER_WAIT_TIMEOUT,
        }
    }

    /// 完了の待ち時間を差し替える。
    #[cfg(test)]
    fn with_wait_timeout(mut self, wait_timeout: Duration) -> Self {
        self.wait_timeout = wait_timeout;
        self
    }
}

impl<H: RenderHost> HostRenderAdapter<H> {
    /// レンダリングを受け付けられる状態かを確かめる。
    ///
    /// 準備前の編集ハンドルはレンダリング API の呼び出し自体が許されないため、
    /// ここを通らない限り [`RenderHost`] の他のメソッドを呼ばない。準備状態の
    /// 問い合わせも捕捉層で包み、どのメソッドから入っても接続の境界まで
    /// 巻き戻らない形を保つ。
    ///
    /// **プレビュー再生中は事前に拒否しない。** レンダリングの投入が失敗する
    /// 条件は「出力中等」としか定められておらず、再生中に描けるかは定まって
    /// いない。描けるのに拒否すれば機能の損失であり、描けないなら投入が失敗して
    /// [`Self::classify_issue_failure`] が拾う。
    fn ensure_renderable(&self) -> Result<(), RenderError> {
        if !catch(|| self.host.is_ready())? {
            return Err(ReadError::NotReady.into());
        }
        match self.edit_state()? {
            EditState::Save => Err(ReadError::EditBlocked {
                state: EditState::Save,
            }
            .into()),
            _ => Ok(()),
        }
    }

    /// 現在の編集状態を取得する。
    fn edit_state(&self) -> Result<EditState, RenderError> {
        guard(|| self.host.edit_state())
    }

    /// 区間の外で編集情報を取得する。
    ///
    /// この取得はフレームレートの分母が 0 のとき落ちる。区間の外、つまり通常の
    /// Rust スレッドで起きるため、捕捉しなければ接続の境界まで巻き戻り、応答を
    /// 返さないまま切断してしまう。1 回のレンダリングでここを 2 回通る。
    fn edit_info(&self) -> Result<HostEditInfo, RenderError> {
        guard(|| self.host.edit_info())
    }

    /// レンダリングタスクを投入する。
    ///
    /// 投入の呼び出し自体も落ち得る。準備状態の確認は受付判定で済ませているが、
    /// 防ぎ漏れに備えて呼び出しを捕捉層で包む。捕捉しなければ接続の境界まで
    /// 巻き戻り、要求元は応答ではなく切断を観測する。
    fn issue(&self, frame: u32, slot: Arc<RenderSlot>) -> Result<(), RenderError> {
        match catch(|| self.host.request_scene_video(frame, slot))? {
            Ok(()) => Ok(()),
            Err(error) => Err(self.classify_issue_failure(error)),
        }
    }

    /// 投入できなかった失敗を、現在の編集状態で分類し直す。
    ///
    /// 投入が失敗する条件は原因を区別せずに畳まれて届くため、判別できるのは
    /// この読み直しだけである。出力中・再生中であれば、時間を置けば解消する
    /// 失敗として返す。読み直しにも失敗した場合は元の分類を保つ。
    fn classify_issue_failure(&self, error: RenderError) -> RenderError {
        match self.edit_state() {
            Ok(state @ (EditState::Save | EditState::Preview)) => {
                ReadError::EditBlocked { state }.into()
            }
            _ => error,
        }
    }
}

/// クロージャの panic を型付きの失敗へ変換し、戻り値はそのまま返す。
fn catch<T>(f: impl FnOnce() -> T) -> Result<T, RenderError> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|_| RenderError::Panicked)
}

/// 失敗を返し得るクロージャの panic を型付きの失敗へ変換する。
fn guard<T>(f: impl FnOnce() -> Result<T, RenderError>) -> Result<T, RenderError> {
    catch(f).and_then(|result| result)
}

impl<H: RenderHost> RenderAdapter for HostRenderAdapter<H> {
    fn render_frame(&self, params: &RenderFrameParams) -> Result<RenderFrameResult, RenderError> {
        self.ensure_renderable()?;
        if !self.inventory.accepts_new_render(Instant::now()) {
            return Err(RenderError::TooManyAbandoned);
        }
        // 受け付けると決まった時点で、取り残された引き渡し用ファイルを掃除する。
        // 専用のスレッドは持たない。
        self.handoff.sweep_expired(SystemTime::now());

        let before = self.edit_info()?;
        ensure_scene(&before, params.expected_scene_id)?;
        ensure_renderable_frame(&before, params.frame)?;

        // 停止の観測を完了待ちだけに任せない。接続受理の停止は待ちきれなければ
        // 要求スレッドを切り離して戻るため、切り離された要求がここまで進むと、
        // **終了手順が在庫を数え終えた後でタスクを増やせる。** 増えた分は誰も
        // 待たず、アンロード後にコールバックが飛ぶ。投入して待ちに入っていれば
        // 在庫に現れるので、塞ぐべき窓は投入の直前だけである。
        if self.stop.is_stop_requested() {
            return Err(RenderError::ShuttingDown);
        }

        let slot = RenderSlot::new(Arc::clone(&self.inventory));
        if let Err(error) = self.issue(params.frame, Arc::clone(&slot)) {
            // ホストへ届かなかったタスクのコールバックは来ない。放棄済みとして
            // 数えると、届かなかった要求が以後の受付を狭めることになる。
            slot.cancel_unissued();
            return Err(error);
        }

        let frame = match slot.wait(Instant::now() + self.wait_timeout, self.stop.as_ref()) {
            SlotWait::Done(result) => result?,
            SlotWait::TimedOut => return Err(RenderError::WaitTimeout),
            SlotWait::Stopped => return Err(RenderError::ShuttingDown),
        };

        // 完了時点のシーンを照合する。符号化も書き出しもこの後に置く。
        let after = self.edit_info()?;
        ensure_scene(&after, params.expected_scene_id)?;

        // epoch と revision は同じ時点から採る。別々の時点から採ると、応答が
        // どの状態の絵かを表せない組になる。
        let project_epoch = self.project.epoch();
        let project_revision = self.project.revision();
        let artifact = self.handoff.write(&frame)?;

        Ok(RenderFrameResult {
            project_epoch,
            project_revision,
            scene_id: after.scene_id,
            frame: frame.frame,
            width: frame.width,
            height: frame.height,
            media_type: ARTIFACT_MEDIA_TYPE.to_string(),
            byte_length: artifact.byte_length,
            sha256: artifact.sha256,
            handoff_token: artifact.token.as_str().to_string(),
        })
    }

    fn discard_artifact(&self, handoff_token: &str) {
        // 応答を送れなかった成果物は、受け取る側が識別子を得ていないため
        // 引き取ることも掃除することもできない。ここで消さなければ、期限切れの
        // 掃除まで残り続ける。
        match HandoffToken::parse(handoff_token) {
            Ok(token) => self.handoff.remove(&token),
            Err(_) => tracing::warn!("引き渡し用ファイルの識別子を解釈できませんでした"),
        }
    }
}

impl<H: RenderHost> RenderDrain for HostRenderAdapter<H> {
    fn outstanding(&self) -> usize {
        self.inventory.outstanding()
    }

    fn wait_all_tasks(&self) {
        self.host.wait_all_tasks();
    }

    fn discard_artifacts(&self) {
        self.handoff.remove_all();
    }
}

/// 現在シーンが要求の前提と一致することを確かめる。
fn ensure_scene(info: &HostEditInfo, expected_scene_id: i32) -> Result<(), RenderError> {
    if info.scene_id == expected_scene_id {
        Ok(())
    } else {
        Err(RenderError::SceneMismatch {
            expected: expected_scene_id,
            current: info.scene_id,
        })
    }
}

/// 編集情報の値とフレーム番号を、投入してよい組かどうかで検証する。
///
/// 検証の順序に意味がある。ホストが返す最終フレームは負値が巨大な値へ化けた
/// ものであり得るため、**それを上限として使う前に範囲そのものを疑う**。逆に
/// すると、化けた上限に対する範囲判定は何も弾かなくなる。
///
/// フレーム番号が受け渡せる範囲に収まることは別に確かめない。上限が範囲内で
/// あることと、フレームが上限以下であることの 2 つから従う。
///
/// 大きさの超過を要求の誤りとして返さないのは、**シーンの解像度は要求元が
/// 選んだものではない**からである。要求を直しても通らない。
///
/// ここで見るのは非圧縮の大きさだけである。符号化後の大きさは画の中身で決まり、
/// 解像度からは求まらないため、この判定を通っても受け渡しの上限を超え得る。
/// その判定は成果物を書き出す直前に行う（[`HandoffDir::write`]）。
fn ensure_renderable_frame(info: &HostEditInfo, frame: u32) -> Result<(), RenderError> {
    let representable = i32::MAX as usize;
    if info.width as usize > representable
        || info.height as usize > representable
        || info.frame_max > representable
    {
        return Err(ReadError::EditInfoOutOfRange.into());
    }
    if frame as usize > info.frame_max {
        return Err(RenderError::FrameOutOfRange);
    }
    if info.width == 0 || info.height == 0 {
        return Err(ReadError::EditInfoOutOfRange.into());
    }
    let frame_bytes = (info.width as u64)
        .checked_mul(info.height as u64)
        .and_then(|pixels| pixels.checked_mul(u64::from(BYTES_PER_PIXEL)))
        .ok_or(RenderError::FrameTooLarge)?;
    if frame_bytes > MAX_RENDER_FRAME_BYTES {
        return Err(RenderError::FrameTooLarge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read::host::ReadHost;
    use crate::render::handoff::{HANDOFF_TTL, HandoffToken};
    use crate::render::slot::{MAX_ABANDONED_RENDERS, deliver_frame_guarded, guard_callback};
    use crate::test_support::with_silent_panic_hook;
    use aviutl2_mcp_core::{
        ARTIFACT_MAX_BYTES, AvailableEffect, ErrorCode, InstanceId, RenderFormat,
    };
    use serde_json::json;
    use std::ops::Deref;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// フェイクが名乗る現在シーン。
    const SCENE_ID: i32 = 3;
    /// フェイクのシーンの最終フレーム。
    const FRAME_MAX: usize = 100;

    /// panic させる位置。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum PanicPoint {
        /// 準備状態の問い合わせ。
        IsReady,
        /// 編集情報の取得。フレームレートの分母が 0 のとき実際に落ちる位置。
        EditInfo,
        /// 投入の呼び出しそのもの。準備前の呼び出しが落ちる位置。
        Issue,
        /// 完了コールバックの内側。
        Callback,
    }

    /// 受け皿をいつ・何回埋めるか。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Completion {
        /// 投入の呼び出しの中で埋める。
        Immediate,
        /// 一度も埋めない。
        Never,
        /// 同じ受け皿を 2 度埋める。
        Twice,
    }

    /// コールバックが返す画像。
    #[derive(Debug, Clone)]
    struct Delivery {
        /// 返すフレーム。`None` なら要求されたフレームをそのまま返す。
        frame: Option<u32>,
        width: u32,
        height: u32,
        pitch: u32,
        pixels: Vec<u8>,
    }

    impl Delivery {
        /// 幅 2・高さ 2・詰め物なしの画像。
        fn sample() -> Self {
            Self {
                frame: None,
                width: 2,
                height: 2,
                pitch: 8,
                pixels: sample_pixels(),
            }
        }

        fn deliver(&self, slot: &RenderSlot, requested_frame: u32) {
            deliver_frame_guarded(
                slot,
                requested_frame,
                self.frame.unwrap_or(requested_frame),
                self.width,
                self.height,
                self.pitch,
                &self.pixels,
            );
        }
    }

    /// 詰め物を除いた既知の画素列。
    fn sample_pixels() -> Vec<u8> {
        vec![
            0xFF, 0x00, 0x00, 0xFF, // 不透明の赤
            0x00, 0xFF, 0x00, 0x80, // 半透明の緑
            0x00, 0x00, 0xFF, 0x00, // 完全に透明な青
            0xFF, 0xFF, 0xFF, 0xFF, // 不透明の白
        ]
    }

    /// SDK の代わりに定型の応答を返すホスト。
    ///
    /// 呼び出された経路を記録するため、受付前に SDK を呼ばないことと、区間へ
    /// 入らないことを検証できる。準備前の呼び出しは、実際の SDK と同じく
    /// 打ち切りで落とす。
    struct FakeRenderHost {
        ready: bool,
        state: EditState,
        /// 2 回目以降の編集状態。投入失敗後の読み直しに使う。
        later_state: Option<EditState>,
        edit_state_calls: AtomicUsize,
        info: HostEditInfo,
        /// 2 回目以降の編集情報。完了後のシーン切り替えを再現する。
        later_info: Option<HostEditInfo>,
        edit_info_calls: AtomicUsize,
        /// 編集情報の取得そのものを失敗させる。
        ///
        /// 返ってきた値が範囲外である場合と作り分けるために持つ。
        edit_info_fails: bool,
        issue_fails: bool,
        panic_at: Option<PanicPoint>,
        completion: Completion,
        delivery: Delivery,
        /// 投入した受け皿。遅れて届くコールバックを後から起こすために持つ。
        slots: Mutex<Vec<Arc<RenderSlot>>>,
        waited: AtomicUsize,
        calls: Mutex<Vec<&'static str>>,
    }

    impl FakeRenderHost {
        fn new() -> Self {
            Self {
                ready: true,
                state: EditState::Edit,
                later_state: None,
                edit_state_calls: AtomicUsize::new(0),
                info: fake_edit_info(),
                later_info: None,
                edit_info_calls: AtomicUsize::new(0),
                edit_info_fails: false,
                issue_fails: false,
                panic_at: None,
                completion: Completion::Immediate,
                delivery: Delivery::sample(),
                slots: Mutex::new(Vec::new()),
                waited: AtomicUsize::new(0),
                calls: Mutex::new(Vec::new()),
            }
        }

        /// 準備前の呼び出しを、実際の SDK と同じ失敗モードで再現する。
        fn assert_ready(&self, api: &str) {
            assert!(self.ready, "準備前に {api} が呼ばれました");
        }

        fn record(&self, call: &'static str) {
            self.calls.lock().unwrap().push(call);
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        fn issued(&self) -> usize {
            self.calls()
                .iter()
                .filter(|call| **call == "request_scene_video")
                .count()
        }

        fn waited(&self) -> usize {
            self.waited.load(Ordering::Acquire)
        }

        /// 忘れられた受け皿へ、遅れてコールバックを届ける。
        fn deliver_pending(&self) {
            for slot in self.slots.lock().unwrap().iter() {
                self.delivery.deliver(slot, 0);
            }
        }

        /// 編集区間へ入る口。レンダリング経路がここへ来ないことを固定するために
        /// 用意してある。
        fn call_edit_section(&self) {
            self.record("call_edit_section");
        }
    }

    impl RenderHost for FakeRenderHost {
        fn is_ready(&self) -> bool {
            assert_ne!(
                self.panic_at,
                Some(PanicPoint::IsReady),
                "準備状態の問い合わせで panic させます"
            );
            self.ready
        }

        fn edit_state(&self) -> Result<EditState, RenderError> {
            self.assert_ready("get_edit_state");
            self.record("edit_state");
            let calls = self.edit_state_calls.fetch_add(1, Ordering::Relaxed);
            Ok(if calls == 0 {
                self.state
            } else {
                self.later_state.unwrap_or(self.state)
            })
        }

        fn edit_info(&self) -> Result<HostEditInfo, RenderError> {
            self.assert_ready("get_edit_info");
            self.record("edit_info");
            assert_ne!(
                self.panic_at,
                Some(PanicPoint::EditInfo),
                "編集情報の取得で panic させます"
            );
            if self.edit_info_fails {
                return Err(RenderError::Sdk {
                    operation: "get_edit_info",
                });
            }
            let calls = self.edit_info_calls.fetch_add(1, Ordering::Relaxed);
            Ok(if calls == 0 {
                self.info.clone()
            } else {
                self.later_info.clone().unwrap_or_else(|| self.info.clone())
            })
        }

        fn request_scene_video(
            &self,
            frame: u32,
            slot: Arc<RenderSlot>,
        ) -> Result<(), RenderError> {
            self.assert_ready("rendering_scene_video");
            self.record("request_scene_video");
            assert_ne!(
                self.panic_at,
                Some(PanicPoint::Issue),
                "投入の呼び出しで panic させます"
            );
            if self.issue_fails {
                return Err(RenderError::Sdk {
                    operation: "rendering_scene_video",
                });
            }
            self.slots.lock().unwrap().push(Arc::clone(&slot));

            if self.panic_at == Some(PanicPoint::Callback) {
                guard_callback(&slot, || panic!("完了コールバックの内側で panic させます"));
                return Ok(());
            }
            match self.completion {
                Completion::Immediate => self.delivery.deliver(&slot, frame),
                Completion::Twice => {
                    self.delivery.deliver(&slot, frame);
                    // 2 度目は要求と違うフレームを返す。上書きされれば、成功では
                    // なく規則の破れが観測される。
                    Delivery {
                        frame: Some(frame.wrapping_add(1)),
                        ..self.delivery.clone()
                    }
                    .deliver(&slot, frame);
                }
                Completion::Never => {}
            }
            Ok(())
        }

        fn wait_all_tasks(&self) {
            self.record("wait_all_tasks");
            self.waited.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// 参照区間へ入る口。
    ///
    /// レンダリングの実行口はこの型を [`RenderHost`] としてしか見ないため、
    /// ここへは到達し得ない。到達しないことを記録で固定する。
    impl ReadHost for FakeRenderHost {
        fn is_ready(&self) -> bool {
            self.ready
        }

        fn edit_state(&self) -> Result<EditState, ReadError> {
            Ok(self.state)
        }

        fn edit_info(&self) -> Result<HostEditInfo, ReadError> {
            Ok(self.info.clone())
        }

        fn effect_catalog(&self) -> Result<Vec<AvailableEffect>, ReadError> {
            Ok(Vec::new())
        }

        fn enter_read_section<T, F>(&self, _f: F) -> Result<T, ReadError>
        where
            T: Send + 'static,
            F: FnOnce(&dyn crate::read::host::SceneReader) -> T + Send,
        {
            self.record("call_read_section");
            Err(ReadError::NotReady)
        }
    }

    fn fake_edit_info() -> HostEditInfo {
        HostEditInfo {
            scene_id: SCENE_ID,
            width: 2,
            height: 2,
            fps_rate: 30000,
            fps_scale: 1001,
            sample_rate: 48000,
            cursor_frame: 0,
            cursor_layer: 0,
            frame_max: FRAME_MAX,
            layer_max: 1,
            display_frame_start: 0,
            display_layer_start: 0,
            display_frame_num: 100,
            display_layer_num: 2,
            select_range_start: None,
            select_range_end: None,
        }
    }

    /// 最初の観測にだけ「停止していない」と答える合図。
    ///
    /// 投入の直前と完了待ちが同じ合図を見るため、待機側の観測だけを確かめる
    /// にはこの形が要る。
    struct StopAfterFirstLook(AtomicUsize);

    impl StopRequest for StopAfterFirstLook {
        fn is_stop_requested(&self) -> bool {
            self.0.fetch_add(1, Ordering::AcqRel) > 0
        }
    }

    /// 一時的な基底ディレクトリ。
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .join(format!("aviutl2-mcp-render-test-{}", InstanceId::new_v4()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 実行口と、その成果物を書く一時ディレクトリ。
    struct Fixture {
        adapter: HostRenderAdapter<FakeRenderHost>,
        /// 実行口より後に解放する。先に消すと書き出し先が無くなる。
        _root: TempRoot,
    }

    impl Deref for Fixture {
        type Target = HostRenderAdapter<FakeRenderHost>;

        fn deref(&self) -> &Self::Target {
            &self.adapter
        }
    }

    /// 停止を要求しない観測口を添えた実行口。
    fn fixture(host: FakeRenderHost) -> Fixture {
        fixture_with(host, Arc::new(AtomicBool::new(false)))
    }

    fn fixture_with(host: FakeRenderHost, stop: Arc<dyn StopRequest>) -> Fixture {
        fixture_full(host, stop, ARTIFACT_MAX_BYTES)
    }

    /// 書き出しの上限を小さくした実行口。
    ///
    /// 上限に掛かる経路を、実際に上限の大きさの画像を作らずに踏ませる。
    fn fixture_with_artifact_cap(host: FakeRenderHost, max_artifact_bytes: u64) -> Fixture {
        fixture_full(host, Arc::new(AtomicBool::new(false)), max_artifact_bytes)
    }

    fn fixture_full(
        host: FakeRenderHost,
        stop: Arc<dyn StopRequest>,
        max_artifact_bytes: u64,
    ) -> Fixture {
        let root = TempRoot::new();
        let handoff = HandoffDir::under(&root.0, &InstanceId::new_v4())
            .with_max_artifact_bytes(max_artifact_bytes);
        let adapter = HostRenderAdapter::new(host, Arc::new(ProjectState::new()), handoff, stop)
            // 期限を待つテストが実時間で数十秒待たないよう短くする。
            .with_wait_timeout(Duration::from_millis(150));
        Fixture {
            adapter,
            _root: root,
        }
    }

    fn params(frame: u32) -> RenderFrameParams {
        RenderFrameParams {
            expected_scene_id: SCENE_ID,
            frame,
            format: RenderFormat::Png,
        }
    }

    /// 成功した結果を取り出す。
    fn rendered(fixture: &Fixture, frame: u32) -> RenderFrameResult {
        fixture
            .render_frame(&params(frame))
            .expect("レンダリングが失敗しました")
    }

    /// 失敗のエラーコードと補助情報を取り出す。
    fn failed(fixture: &Fixture, frame: u32) -> (ErrorCode, serde_json::Value) {
        let error = fixture
            .render_frame(&params(frame))
            .expect_err("レンダリングが成功しました");
        (error.error_code(), error.details())
    }

    #[test]
    fn a_render_carries_the_scene_the_frame_and_the_project_state() {
        let fixture = fixture(FakeRenderHost::new());
        let result = rendered(&fixture, 7);

        assert_eq!(result.scene_id, SCENE_ID);
        assert_eq!(result.frame, 7);
        assert_eq!(result.width, 2);
        assert_eq!(result.height, 2);
        // 引き取る側は同じ media type を独立に名乗る。期待は取り決めそのものを
        // 書き下し、共有の定義だけが変わったときに書き出す側でも落ちるようにする。
        assert_eq!(result.media_type, "image/png");
        assert_eq!(result.project_epoch, fixture.project.epoch());
        assert_eq!(result.project_revision, fixture.project.revision());
        assert!(result.byte_length > 0);
        assert!(result.sha256.starts_with("sha256:"));
        assert_eq!(fixture.handoff.entry_count(), 1);
        assert_eq!(
            RenderDrain::outstanding(&fixture.adapter),
            0,
            "完了したタスクが在庫に残っています"
        );
    }

    #[test]
    fn a_render_while_starting_is_refused_without_calling_the_sdk() {
        let fixture = fixture(FakeRenderHost {
            ready: false,
            ..FakeRenderHost::new()
        });

        let (code, details) = failed(&fixture, 7);
        assert_eq!(code, ErrorCode::HostBusy);
        assert_eq!(details["retry_requires"], json!("resend"));
        assert!(
            fixture.host.calls().is_empty(),
            "準備前に SDK を呼び出しました: {:?}",
            fixture.host.calls()
        );
    }

    #[test]
    fn a_render_while_saving_is_refused_without_issuing() {
        let fixture = fixture(FakeRenderHost {
            state: EditState::Save,
            ..FakeRenderHost::new()
        });

        let (code, details) = failed(&fixture, 7);
        assert_eq!(code, ErrorCode::EditBlocked);
        assert_eq!(details["edit_state"], json!("save"));
        let calls = fixture.host.calls();
        assert!(
            !calls.contains(&"request_scene_video"),
            "出力中に投入しました: {calls:?}"
        );
        assert!(
            !calls.contains(&"edit_info"),
            "出力中に編集情報を取得しました: {calls:?}"
        );
    }

    #[test]
    fn a_render_while_previewing_is_not_refused_up_front() {
        // 投入が失敗する条件は「出力中等」としか定められていない。再生中に
        // 描けるなら、事前に拒否するのは機能の損失である。描けないなら投入が
        // 失敗し、状態の読み直しが拾う。
        let fixture = fixture(FakeRenderHost {
            state: EditState::Preview,
            ..FakeRenderHost::new()
        });

        assert_eq!(rendered(&fixture, 7).frame, 7);
        assert_eq!(fixture.host.issued(), 1);
    }

    #[test]
    fn an_issue_failure_is_reclassified_from_the_state_read_back() {
        for state in [EditState::Save, EditState::Preview] {
            let fixture = fixture(FakeRenderHost {
                issue_fails: true,
                later_state: Some(state),
                ..FakeRenderHost::new()
            });

            let (code, details) = failed(&fixture, 7);
            assert_eq!(code, ErrorCode::EditBlocked, "{state} で分類されません");
            assert_eq!(details["edit_state"], json!(state.as_str()));
        }
    }

    #[test]
    fn an_issue_failure_while_editing_stays_a_sdk_error() {
        let fixture = fixture(FakeRenderHost {
            issue_fails: true,
            ..FakeRenderHost::new()
        });

        let (code, details) = failed(&fixture, 7);
        assert_eq!(code, ErrorCode::SdkError);
        assert_eq!(details["sdk_operation"], json!("rendering_scene_video"));
    }

    #[test]
    fn a_render_that_never_reached_the_host_does_not_narrow_later_admissions() {
        // 届かなかったタスクのコールバックは来ない。放棄済みとして数えると、
        // 届かなかった要求が以後の受付を狭めることになる。
        let fixture = fixture(FakeRenderHost {
            issue_fails: true,
            ..FakeRenderHost::new()
        });

        for _ in 0..MAX_ABANDONED_RENDERS * 2 {
            assert_eq!(failed(&fixture, 7).0, ErrorCode::SdkError);
        }
        assert_eq!(RenderDrain::outstanding(&fixture.adapter), 0);
        assert!(fixture.inventory.accepts_new_render(Instant::now()));
    }

    #[test]
    fn a_scene_mismatch_before_issuing_never_calls_the_sdk() {
        let fixture = fixture(FakeRenderHost::new());
        let error = fixture
            .render_frame(&RenderFrameParams {
                expected_scene_id: SCENE_ID + 1,
                ..params(7)
            })
            .expect_err("別シーンの要求が成功しました");

        assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
        assert_eq!(error.details()["mismatch"], json!("scene_id"));
        assert_eq!(error.details()["current_scene_id"], json!(SCENE_ID));
        assert_eq!(fixture.host.issued(), 0, "シーンが違うのに投入しました");
    }

    #[test]
    fn a_scene_switched_during_the_render_discards_the_artifact() {
        // 完了後の照合は符号化と書き出しの前に置く。捨てると決まっているものへ
        // 入出力の費用を払わない。
        let fixture = fixture(FakeRenderHost {
            later_info: Some(HostEditInfo {
                scene_id: SCENE_ID + 1,
                ..fake_edit_info()
            }),
            ..FakeRenderHost::new()
        });

        let (code, details) = failed(&fixture, 7);
        assert_eq!(code, ErrorCode::PreconditionFailed);
        assert_eq!(details["current_scene_id"], json!(SCENE_ID + 1));
        assert_eq!(
            fixture.handoff.entry_count(),
            0,
            "捨てる成果物を書き出しました"
        );
    }

    #[test]
    fn a_frame_past_the_end_of_the_scene_is_an_invalid_argument() {
        let fixture = fixture(FakeRenderHost::new());
        let (code, details) = failed(&fixture, FRAME_MAX as u32 + 1);

        assert_eq!(code, ErrorCode::InvalidArgument);
        assert_eq!(details["reason"], json!("frame_out_of_range"));
        assert_eq!(fixture.host.issued(), 0, "範囲外のフレームを投入しました");
        // 最終フレームちょうどは範囲内である。
        assert_eq!(rendered(&fixture, FRAME_MAX as u32).frame, FRAME_MAX as u32);
    }

    #[test]
    fn a_folded_edit_info_is_never_used_as_a_bound() {
        // 負値を畳んだ値は 2^31 以上の巨大な値として届く。これを上限として
        // 使うと、フレームの範囲判定が何も弾かなくなる。上限として使う前に
        // 範囲そのものを疑う。
        for info in [
            HostEditInfo {
                frame_max: i32::MAX as usize + 1,
                ..fake_edit_info()
            },
            HostEditInfo {
                width: i32::MAX as u32 + 1,
                ..fake_edit_info()
            },
            HostEditInfo {
                height: i32::MAX as u32 + 1,
                ..fake_edit_info()
            },
        ] {
            let fixture = fixture(FakeRenderHost {
                info,
                ..FakeRenderHost::new()
            });
            let (code, details) = failed(&fixture, 7);
            assert_eq!(code, ErrorCode::SdkError);
            assert_eq!(details["sdk_operation"], json!("get_edit_info"));
            assert_eq!(details["reason"], json!("edit_info_out_of_range"));
            assert_eq!(
                fixture.host.issued(),
                0,
                "信頼できない編集情報のまま投入しました"
            );
        }
    }

    #[test]
    fn a_failed_edit_info_call_is_told_apart_from_an_out_of_range_value() {
        // どちらも同じ関数を名指しする sdk_error として返る。名前が付かなければ
        // 要求元も運用者も、呼び出しが失敗したのかホストが壊れた値を返したのかを
        // 切り分けられない。
        let call_failed = fixture(FakeRenderHost {
            edit_info_fails: true,
            ..FakeRenderHost::new()
        });
        let (call_code, call_details) = failed(&call_failed, 7);
        assert_eq!(call_code, ErrorCode::SdkError);
        assert_eq!(call_details["sdk_operation"], json!("get_edit_info"));
        assert!(
            call_details.get("reason").is_none(),
            "呼び出しの失敗に名前が付きました: {call_details}"
        );

        let out_of_range = fixture(FakeRenderHost {
            info: HostEditInfo {
                width: i32::MAX as u32 + 1,
                ..fake_edit_info()
            },
            ..FakeRenderHost::new()
        });
        let (value_code, value_details) = failed(&out_of_range, 7);
        assert_eq!(value_code, call_code);
        assert_eq!(
            value_details["sdk_operation"],
            call_details["sdk_operation"]
        );
        assert_eq!(value_details["reason"], json!("edit_info_out_of_range"));
    }

    #[test]
    fn a_scene_without_a_size_is_a_sdk_error() {
        for info in [
            HostEditInfo {
                width: 0,
                ..fake_edit_info()
            },
            HostEditInfo {
                height: 0,
                ..fake_edit_info()
            },
        ] {
            let fixture = fixture(FakeRenderHost {
                info,
                ..FakeRenderHost::new()
            });
            assert_eq!(failed(&fixture, 7).0, ErrorCode::SdkError);
        }
    }

    #[test]
    fn a_scene_too_large_to_hold_is_unsupported_rather_than_invalid() {
        // 解像度は要求元が選んだ値ではない。要求を直しても通らないため、
        // 要求の誤りとしては返さない。
        let fixture = fixture(FakeRenderHost {
            info: HostEditInfo {
                width: 8 * 1024,
                height: 8 * 1024 + 1,
                ..fake_edit_info()
            },
            ..FakeRenderHost::new()
        });

        let (code, details) = failed(&fixture, 7);
        assert_eq!(code, ErrorCode::UnsupportedOperation);
        assert_eq!(details["reason"], json!("frame_too_large"));
        assert_eq!(
            fixture.host.issued(),
            0,
            "抱えきれない大きさのまま投入しました"
        );
    }

    #[test]
    fn an_artifact_too_large_to_hand_over_is_unsupported_rather_than_internal() {
        // 符号化後の大きさは画の中身で決まり、投入前に見る非圧縮の上限では
        // 決まらない。受け取る側が引き取れない大きさをそのまま書き出すと、
        // 要求元は引き取りの失敗（内部エラー）しか受け取れず、何が起きたのかも
        // どうすれば通るのかも分からない。書き出す前に落とし、直しても通らない
        // ものとして返す。
        let fixture = fixture_with_artifact_cap(FakeRenderHost::new(), 1);

        let (code, details) = failed(&fixture, 7);
        assert_eq!(code, ErrorCode::UnsupportedOperation);
        assert_eq!(details["reason"], json!("frame_too_large"));
        assert_eq!(
            fixture.handoff.entry_count(),
            0,
            "引き取れない成果物を書き出しました"
        );
    }

    #[test]
    fn a_callback_that_never_arrives_leaves_on_the_deadline() {
        let fixture = fixture(FakeRenderHost {
            completion: Completion::Never,
            ..FakeRenderHost::new()
        });

        let started = Instant::now();
        let (code, details) = failed(&fixture, 7);
        assert_eq!(code, ErrorCode::Timeout);
        assert_eq!(details["render_stage"], json!("wait"));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "期限を大きく超えて待ち続けました: {}ms",
            started.elapsed().as_millis()
        );
        assert_eq!(
            RenderDrain::outstanding(&fixture.adapter),
            1,
            "放棄してもホスト側のタスクは生きている"
        );
    }

    #[test]
    fn a_stop_request_leaves_the_wait_early_as_host_busy() {
        // 要求処理は接続を受けるスレッド上で同期実行される。停止を観測できない
        // 待ちにすると、終了のたびにそのスレッドを切り離すことになる。
        //
        // 投入の直前でも停止を見るため、待機側の観測だけを確かめるには、
        // 1 度目の観測で停止していないと答える合図が要る。
        let fixture = fixture_with(
            FakeRenderHost {
                completion: Completion::Never,
                ..FakeRenderHost::new()
            },
            Arc::new(StopAfterFirstLook(AtomicUsize::new(0))),
        );

        let started = Instant::now();
        let (code, details) = failed(&fixture, 7);
        assert_eq!(code, ErrorCode::HostBusy);
        assert!(details["retry_after_ms"].is_number());
        assert_eq!(fixture.host.issued(), 1, "完了待ちまで進んでいません");
        assert!(
            started.elapsed() < Duration::from_millis(150),
            "停止要求を観測できていません: {}ms",
            started.elapsed().as_millis()
        );
    }

    #[test]
    fn a_stop_request_keeps_the_task_from_being_issued_at_all() {
        // 接続受理の停止は待ちきれなければ要求スレッドを切り離して戻る。
        // 切り離された要求が投入まで進むと、終了手順が在庫を数え終えた後に
        // タスクが増える。増えた分は誰も待たないままアンロードされる。
        let fixture = fixture_with(FakeRenderHost::new(), Arc::new(AtomicBool::new(true)));

        let (code, details) = failed(&fixture, 7);
        assert_eq!(code, ErrorCode::HostBusy);
        assert!(details["retry_after_ms"].is_number());
        assert_eq!(
            fixture.host.issued(),
            0,
            "停止が要求された後にタスクを投入しました"
        );
        assert_eq!(RenderDrain::outstanding(&fixture.adapter), 0);
    }

    #[test]
    fn a_new_render_sweeps_what_earlier_ones_left_behind() {
        // 正常時、引き渡し用ファイルは受け取る側が引き取った直後に消える。
        // 残るのは失敗経路だけであり、その掃除は新しい要求を受けたときに行う。
        // 専用のスレッドを持たないため、要求経路から呼ばれなければ誰も掃除しない。
        let fixture = fixture(FakeRenderHost::new());
        let stale = HandoffToken::parse(&rendered(&fixture, 7).handoff_token).unwrap();
        fixture
            .handoff
            .age_entries(HANDOFF_TTL + Duration::from_secs(1));

        let fresh = HandoffToken::parse(&rendered(&fixture, 8).handoff_token).unwrap();

        assert!(
            fixture.handoff.read_artifact(&stale).is_none(),
            "期限を過ぎた引き渡し用ファイルが残りました"
        );
        assert!(
            fixture.handoff.read_artifact(&fresh).is_some(),
            "書いたばかりの成果物まで消えました"
        );
        assert_eq!(fixture.handoff.entry_count(), 1);
    }

    #[test]
    fn a_late_callback_frees_the_admission_without_leaving_an_image() {
        let fixture = fixture(FakeRenderHost {
            completion: Completion::Never,
            ..FakeRenderHost::new()
        });

        for _ in 0..MAX_ABANDONED_RENDERS {
            assert_eq!(failed(&fixture, 0).0, ErrorCode::Timeout);
        }

        let (code, details) = failed(&fixture, 0);
        assert_eq!(code, ErrorCode::HostBusy, "上限に達しても受け付けました");
        assert!(details["retry_after_ms"].is_number());
        assert_eq!(
            fixture.host.issued(),
            MAX_ABANDONED_RENDERS,
            "上限に達した後も投入しました"
        );

        // 忘れたタスクのコールバックが遅れて届く。
        fixture.host.deliver_pending();

        assert_eq!(
            fixture.handoff.entry_count(),
            0,
            "放棄済みの受け皿へ届いた画像が書き出されました"
        );
        assert_eq!(RenderDrain::outstanding(&fixture.adapter), 0);
        assert_eq!(
            failed(&fixture, 0).0,
            ErrorCode::Timeout,
            "到着で計上が減っても受付が戻りません"
        );
        assert_eq!(
            fixture.host.issued(),
            MAX_ABANDONED_RENDERS + 1,
            "受付は戻ったのに投入されていません"
        );
    }

    #[test]
    fn a_second_callback_does_not_change_the_result() {
        let fixture = fixture(FakeRenderHost {
            completion: Completion::Twice,
            ..FakeRenderHost::new()
        });

        assert_eq!(rendered(&fixture, 7).frame, 7);
        assert_eq!(RenderDrain::outstanding(&fixture.adapter), 0);
    }

    #[test]
    fn a_panicking_callback_fails_the_request_without_making_it_wait() {
        let fixture = fixture(FakeRenderHost {
            panic_at: Some(PanicPoint::Callback),
            ..FakeRenderHost::new()
        });

        let started = Instant::now();
        let code = with_silent_panic_hook(|| failed(&fixture, 7).0);
        assert_eq!(code, ErrorCode::InternalError);
        assert!(
            started.elapsed() < Duration::from_millis(150),
            "panic を捕捉したのに期限まで待ちました: {}ms",
            started.elapsed().as_millis()
        );
    }

    #[test]
    fn a_panic_outside_the_callback_becomes_an_internal_error() {
        // 準備状態の問い合わせ・編集情報の取得・投入の呼び出しは、いずれも
        // 通常の Rust スレッドで落ち得る。捕捉しなければ接続の境界まで巻き戻り、
        // 要求元は応答ではなく切断を観測する。
        for point in [PanicPoint::IsReady, PanicPoint::EditInfo, PanicPoint::Issue] {
            let fixture = fixture(FakeRenderHost {
                panic_at: Some(point),
                ..FakeRenderHost::new()
            });

            let code = with_silent_panic_hook(|| failed(&fixture, 7).0);
            assert_eq!(code, ErrorCode::InternalError, "{point:?} で捕捉されません");
            assert_eq!(
                RenderDrain::outstanding(&fixture.adapter),
                0,
                "{point:?} の panic で在庫が残りました"
            );
        }
    }

    #[test]
    fn a_padded_buffer_is_written_with_its_padding_removed() {
        // pitch は行の詰め物を許すために存在する値であり、詰め物がある方が
        // 正常であり得る。既知の画素列で往復を確かめる。
        const PAD: u8 = 0xEE;
        let mut padded = Vec::new();
        for row in sample_pixels().chunks(8) {
            padded.extend_from_slice(row);
            padded.extend_from_slice(&[PAD; 4]);
        }
        let fixture = fixture(FakeRenderHost {
            delivery: Delivery {
                pitch: 12,
                pixels: padded,
                ..Delivery::sample()
            },
            ..FakeRenderHost::new()
        });

        let result = rendered(&fixture, 7);
        let token = HandoffToken::parse(&result.handoff_token).expect("識別子が復元できません");
        let written = fixture
            .handoff
            .read_artifact(&token)
            .expect("成果物が読めません");

        let decoder = png::Decoder::new(std::io::Cursor::new(written));
        let mut reader = decoder.read_info().expect("PNG として読めません");
        let mut decoded = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut decoded).expect("画素が読めません");
        assert_eq!(
            &decoded[..info.buffer_size()],
            &sample_pixels()[..],
            "詰め物を除いた画素が往復しません"
        );
    }

    #[test]
    fn a_buffer_that_breaks_a_rule_is_a_sdk_error_naming_the_rule() {
        let fixture = fixture(FakeRenderHost {
            delivery: Delivery {
                // 要求と違うフレームを返す。
                frame: Some(8),
                ..Delivery::sample()
            },
            ..FakeRenderHost::new()
        });

        let (code, details) = failed(&fixture, 7);
        assert_eq!(code, ErrorCode::SdkError);
        assert_eq!(details["reason"], json!("frame_mismatch"));
        assert_eq!(
            fixture.handoff.entry_count(),
            0,
            "検証に落ちた画像を書き出しました"
        );
    }

    #[test]
    fn a_render_never_enters_a_read_or_edit_section() {
        // 完了待ちを参照ロック・編集ロックの下で呼ぶとデッドロックし得る。
        // 区間へ入る口はフェイクにだけ用意してあり、レンダリングの SDK 境界には
        // 存在しない。実際に呼ばれないことを記録で固定する。
        let fixture = fixture(FakeRenderHost::new());
        rendered(&fixture, 7);
        assert!(
            !fixture
                .host
                .calls()
                .iter()
                .any(|call| matches!(*call, "call_read_section" | "call_edit_section")),
            "レンダリングが区間へ入りました: {:?}",
            fixture.host.calls()
        );

        // 口そのものは生きている。呼べば記録に現れる。
        ReadHost::enter_read_section(&fixture.adapter.host, |_| ())
            .expect_err("フェイクの参照区間が成功しました");
        fixture.host.call_edit_section();
        assert_eq!(
            fixture
                .host
                .calls()
                .iter()
                .filter(|call| matches!(**call, "call_read_section" | "call_edit_section"))
                .count(),
            2
        );
    }

    #[test]
    fn an_unsent_artifact_is_removed() {
        // 送信そのものに失敗した成果物は、受け取る側が識別子を得ていないため
        // 引き取ることも掃除することもできない。
        let fixture = fixture(FakeRenderHost::new());
        let result = rendered(&fixture, 7);
        assert_eq!(fixture.handoff.entry_count(), 1);

        fixture.discard_artifact(&result.handoff_token);
        assert_eq!(fixture.handoff.entry_count(), 0);

        // 解釈できない識別子でも落とさない。掃除は要求を失敗させない。
        fixture.discard_artifact("..");
    }

    #[test]
    fn the_shutdown_view_counts_running_renders_and_clears_the_directory() {
        let fixture = fixture(FakeRenderHost {
            completion: Completion::Never,
            ..FakeRenderHost::new()
        });
        assert_eq!(RenderDrain::outstanding(&fixture.adapter), 0);

        assert_eq!(failed(&fixture, 7).0, ErrorCode::Timeout);
        assert_eq!(
            RenderDrain::outstanding(&fixture.adapter),
            1,
            "終了判定が放棄済みのタスクを見落としています"
        );

        RenderDrain::wait_all_tasks(&fixture.adapter);
        assert_eq!(fixture.host.waited(), 1);

        RenderDrain::discard_artifacts(&fixture.adapter);
        assert_eq!(fixture.handoff.entry_count(), 0);
    }
}
