//! 完了コールバックと要求元が共有する受け皿と、投入したタスクの在庫。
//!
//! # なぜ受け皿を共有し、放棄できる形にするのか
//!
//! SDK ラッパーへ渡したクロージャは、**完了コールバック自身が解放する**。
//! したがってコールバックが一度も呼ばれなければクロージャは永久に残り、
//! そこへ結果の受け渡し口を直接持たせると、対応する待ちは永久に返らない。
//! ラッパー側に期限は無い。
//!
//! そのため待ちの期限は利用側に置き、クロージャが捕捉するのは
//! **放棄できる共有の受け皿だけ**にする。要求元は期限を過ぎたら受け皿を放棄し、
//! 放棄後に到着したコールバックは結果を捨てて戻る。
//!
//! # なぜ受け皿が在庫を持つのか
//!
//! 放棄したタスクはホスト側でまだ生きており、取り消す手段が無い。したがって
//! 「ホストが抱えている未完了タスクの数」は 1 を超え得る。この数を縛らないと
//! 放棄済みの受け皿とクロージャが無制限に積み上がり、終了時に待つべき
//! タスクも増える。
//!
//! **数を減らせる唯一の経路が、放棄済みの受け皿へコールバックが到着すること
//! である。** 受け皿から在庫へ到達できなければ計上は単調増加になり、
//! [`MAX_ABANDONED_RENDERS`] 回の期限超過でレンダリングがプロセスの寿命の間
//! ずっと使えなくなる。

use crate::render::buffer;
use crate::render::error::RenderError;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// 受付を止める放棄済みタスクの数。
pub const MAX_ABANDONED_RENDERS: usize = 4;

/// 放棄の計上をあきらめるまでの時間。
///
/// 完了コールバックの待ち時間より十分長く採る。遅れて完了したタスクを
/// 取りこぼさないための余裕であり、減衰が効くのは本当に来ないタスクだけである。
pub const ABANDONED_ENTRY_TTL: Duration = Duration::from_secs(300);

/// 待機が目を覚ます間隔。
///
/// 完了だけでなく、期限と停止要求も見る必要があるため、通知だけに頼らない。
pub const RENDER_WAIT_TICK: Duration = Duration::from_millis(100);

/// 詰め物を除いた RGBA8 画像。
#[derive(Clone, PartialEq, Eq)]
pub struct RenderedFrame {
    /// 描画したフレーム。
    pub frame: u32,
    /// 画像の幅（画素）。
    pub width: u32,
    /// 画像の高さ（画素）。
    pub height: u32,
    /// 長さは `width * height * 4`。
    pub pixels: Vec<u8>,
}

/// 画素を出さない表示。
///
/// 画像には利用者のプロジェクトの内容が写る。導出した表示のままにすると、
/// この型を含む値をどこかで表示に流した時点で画素列がそのまま出る。表示する
/// 場所を後から全て見張るより、型の側で出せなくしておく。これは
/// [`SlotWait`] のように本型を含む値の表示にも効く。
impl std::fmt::Debug for RenderedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderedFrame")
            .field("frame", &self.frame)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pixel_bytes", &self.pixels.len())
            .finish()
    }
}

/// 受け皿の状態。
#[derive(Debug)]
enum SlotState {
    /// 投入済み、未完了。
    Pending,
    /// コールバックが結果を置いた。要求元が回収すると中身は空になる。
    Done(Option<Result<RenderedFrame, RenderError>>),
    /// 要求元が放棄した。**終端状態**であり、ここから遷移しない。
    Abandoned {
        /// コールバックが到着済みか。
        ///
        /// 在庫を二重に減らさないために持つ。ホストが同じタスクのコールバックを
        /// 2 度呼んだ場合、2 度目で計上を減らすと在庫が実態より少なくなる。
        callback_arrived: bool,
    },
}

/// 放棄済みタスク 1 件分の記録。
#[derive(Debug, Clone, Copy)]
struct AbandonedEntry {
    /// 放棄した時刻。受付判定の計上を時間で減衰させるために持つ。
    abandoned_at: Instant,
}

/// 投入したタスクの在庫。要求処理側と全ての受け皿が共有する。
#[derive(Debug, Default)]
pub struct RenderInventory {
    /// 投入済みで、まだ完了も放棄もしていない件数。
    pending: AtomicUsize,
    /// 放棄され、まだコールバックが到着していない記録。
    abandoned: Mutex<Vec<AbandonedEntry>>,
}

impl RenderInventory {
    /// 空の在庫を作る。
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 受付判定に使う放棄済み件数。
    ///
    /// [`ABANDONED_ENTRY_TTL`] を過ぎた記録は数えない。数え続けると、
    /// ホストが永久に完了させないタスクが [`MAX_ABANDONED_RENDERS`] 件たまった
    /// 時点で、レンダリングが二度と使えなくなる。忘れてよいのは「新しい要求を
    /// 受けるかどうか」の判断だけである。
    pub fn abandoned_within_ttl(&self, now: Instant) -> usize {
        self.abandoned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|entry| now.saturating_duration_since(entry.abandoned_at) < ABANDONED_ENTRY_TTL)
            .count()
    }

    /// 新しい要求を受け付けてよいか。
    pub fn accepts_new_render(&self, now: Instant) -> bool {
        self.abandoned_within_ttl(now) < MAX_ABANDONED_RENDERS
    }

    /// 終了判定に使う在庫の全件。
    ///
    /// **時間による減衰を適用しない。** 忘れたタスクも、生きているならアンロード
    /// 後に飛んでくる。受付を止めるのは利用者の不便で済むが、アンロードしてよいか
    /// の判断を誤るとプロセスが落ちる。
    ///
    /// 実行中（未完了）のタスクを数えることが要である。停止要求の観測が遅れても、
    /// 待機側が放棄を記録できないまま終わっても、投入時点で増えた分は残る。
    pub fn outstanding(&self) -> usize {
        self.pending.load(Ordering::Acquire)
            + self
                .abandoned
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len()
    }

    fn record_issue(&self) {
        self.pending.fetch_add(1, Ordering::AcqRel);
    }

    fn release_issue(&self) {
        let _ = self
            .pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_sub(1))
            });
    }

    fn record_abandon(&self, now: Instant) {
        self.abandoned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(AbandonedEntry { abandoned_at: now });
    }

    /// 放棄済みの記録を 1 件落とす。
    ///
    /// **落とすのは最も新しい 1 件である。** どの記録がどのタスクのものかは
    /// 分からないため、どれを落としても総数は同じだけ減る。違うのは受付判定への
    /// 効き方である。
    ///
    /// 記録は放棄した順に並ぶ。最古を落とすと、そこに溜まっているのは
    /// [`ABANDONED_ENTRY_TTL`] を過ぎて**もともと受付判定に数えられていない**
    /// 記録であり、到着しても受付判定の件数が減らない。期限切れが 4 件、
    /// TTL 内が 4 件たまった状態では、TTL 内のタスクのコールバックが何件届いても
    /// 受付が戻らなくなる。
    ///
    /// 最新を落とせばこれが起きない。受付判定の件数が 0 でないなら、記録の順序
    /// から最新は必ず TTL 内であり、落とせば必ず 1 減る。
    fn release_abandon(&self) {
        let mut abandoned = self.abandoned.lock().unwrap_or_else(|e| e.into_inner());
        abandoned.pop();
    }
}

/// 停止が要求されたかどうかだけを見る観測口。
///
/// 待機が観測する停止は、接続受理ループを止めるものと**同一でなければ
/// ならない**。要求処理は接続を受けるスレッド上で同期実行されるため、別の
/// 合図を用意すると、片方が立ってからもう片方が立つまでの間に待機が居座る。
/// 停止手順はその間ずっと待たされ、待ちきれずスレッドを切り離すことになる。
///
/// 観測だけを型として切り出すのは、待機に停止の**立て方**を渡さないためである。
/// 立てられる値を渡すと、レンダリング側から停止を起こす経路が生まれる。
/// 実装は [`crate::pipe::StopSignal`] に対して下で与えており、それ以外の合図を
/// 新たに作る必要は無い。
pub trait StopRequest: Send + Sync {
    /// 停止が要求済みか。待機なしで確認できること。
    fn is_stop_requested(&self) -> bool;
}

impl StopRequest for crate::pipe::StopSignal {
    fn is_stop_requested(&self) -> bool {
        self.is_signaled()
    }
}

/// 合図を差し替えて待機を確かめるための実装。
///
/// 本番では [`crate::pipe::StopSignal`] を渡す。こちらは Win32 のイベントを
/// 用意せずに停止の観測だけを再現できる。
impl StopRequest for AtomicBool {
    fn is_stop_requested(&self) -> bool {
        self.load(Ordering::Acquire)
    }
}

/// 期限付き待機の脱出理由。
#[derive(Debug)]
pub enum SlotWait {
    /// コールバックが結果を置いた。
    Done(Result<RenderedFrame, RenderError>),
    /// 期限を過ぎた。受け皿は放棄済み。
    TimedOut,
    /// 停止が要求された。受け皿は放棄済み。
    Stopped,
}

/// 完了コールバックと要求元が共有する受け皿。
///
/// クロージャが直接捕捉するのはこれだけである。ここから在庫へ到達できることが、
/// 放棄の計上を減らせる唯一の経路になる。
#[derive(Debug)]
pub struct RenderSlot {
    state: Mutex<SlotState>,
    ready: Condvar,
    inventory: Arc<RenderInventory>,
}

impl RenderSlot {
    /// 未完了の受け皿を作り、在庫へ 1 件計上する。
    ///
    /// 計上を投入の成功後ではなくここで行うのは、投入の呼び出しが戻る前に
    /// コールバックが走り得るためである。戻ってから数えると、先に完了した分が
    /// 計上を下回り、在庫が実態とずれる。投入に失敗した場合は
    /// [`RenderSlot::cancel_unissued`] で取り消す。
    pub fn new(inventory: Arc<RenderInventory>) -> Arc<Self> {
        inventory.record_issue();
        Arc::new(Self {
            state: Mutex::new(SlotState::Pending),
            ready: Condvar::new(),
            inventory,
        })
    }

    /// 毒された状態を無視して受け皿の状態を取る。
    ///
    /// 状態のロックを毒したままにすると、以後この受け皿へ触れる全ての経路が
    /// panic する。コールバックはホストのレンダリング用スレッドで走るため、
    /// そこで panic を起こすとプロセスごと落ちる。
    fn lock_state(&self) -> MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 既に放棄されているか。
    pub fn is_abandoned(&self) -> bool {
        matches!(*self.lock_state(), SlotState::Abandoned { .. })
    }

    /// コールバックの到着を記録し、結果を作る必要が無いなら `true` を返す。
    ///
    /// 放棄済みなら在庫から自分を落として早期に戻る。放棄は期限超過で起きる
    /// ため、ホストが遅れている場面と相関する。そこで最大サイズの確保と複製を
    /// まるごと省ける。
    fn note_callback_arrival(&self) -> bool {
        let mut state = self.lock_state();
        match &mut *state {
            SlotState::Abandoned { callback_arrived } => {
                if !*callback_arrived {
                    *callback_arrived = true;
                    self.inventory.release_abandon();
                }
                true
            }
            SlotState::Done(_) => true,
            SlotState::Pending => false,
        }
    }

    /// コールバックが結果を置く。
    ///
    /// 放棄済みなら結果を捨てる。放棄された受け皿に画像を残すと、回収されない
    /// 受け皿がそのまま大きなメモリを抱え込む。既に結果がある場合も上書き
    /// しない。ホストが同じタスクのコールバックを 2 度呼んでも、少なくとも値の
    /// 整合は壊さない。
    fn complete(&self, result: Result<RenderedFrame, RenderError>) {
        let mut state = self.lock_state();
        match &mut *state {
            SlotState::Pending => {
                *state = SlotState::Done(Some(result));
                self.inventory.release_issue();
                drop(state);
                self.ready.notify_all();
            }
            SlotState::Abandoned { callback_arrived } => {
                if !*callback_arrived {
                    *callback_arrived = true;
                    self.inventory.release_abandon();
                }
            }
            SlotState::Done(_) => {}
        }
    }

    /// 要求元が受け皿を放棄する。
    ///
    /// 放棄は終端であり、取り消せない。取り消せると、期限超過を返した要求が
    /// 後から成功する経路が生まれる。
    ///
    /// **放棄済みへの計上を先に行い、未完了からの取り下げを後に行う。順序を
    /// 逆にしてはならない。** [`RenderInventory::outstanding`] は未完了の件数を
    /// 読んでから放棄済みの記録を数えるため、逆順にすると 2 つの更新の隙間で
    /// 在庫が 0 に見える。終了判定がその瞬間を読むと、ホスト側でタスクが生きて
    /// いるのに待たずにアンロードへ進む。放棄は停止要求でも起きるため、
    /// 終了手順の中で在庫を数えるのとまさに並行する。
    ///
    /// この順序なら隙間で起きるのは過大計上であり、余分に待つだけで済む。
    fn abandon_at(&self, now: Instant) {
        let mut state = self.lock_state();
        if let SlotState::Pending = *state {
            *state = SlotState::Abandoned {
                callback_arrived: false,
            };
            self.inventory.record_abandon(now);
            self.inventory.release_issue();
        }
    }

    /// 投入に失敗した受け皿を在庫から落とす。
    ///
    /// コールバックは来ないため、放棄済みとしては数えない。数えると、
    /// ホストへ届かなかった要求が以後の受付を狭めることになる。
    pub fn cancel_unissued(&self) {
        let mut state = self.lock_state();
        if let SlotState::Pending = *state {
            *state = SlotState::Abandoned {
                callback_arrived: true,
            };
            self.inventory.release_issue();
        }
    }

    /// 完了・期限・停止要求のいずれかまで待つ。
    ///
    /// 単純な通知待ちにしないのは、完了だけでなく期限と停止要求も見る必要が
    /// あるためである。要求処理は接続を受けるスレッド上で同期実行されるため、
    /// 停止要求を観測できない待ちにすると、終了のたびにそのスレッドを切り離す
    /// ことになる。
    ///
    /// 期限超過と停止要求では、戻る前に受け皿を放棄する。放棄を呼び出し側の
    /// 作法に委ねると、忘れた経路で在庫が減らないまま残る。
    pub fn wait(&self, deadline: Instant, stop: &dyn StopRequest) -> SlotWait {
        let mut state = self.lock_state();
        loop {
            if let SlotState::Done(result) = &mut *state
                && let Some(result) = result.take()
            {
                return SlotWait::Done(result);
            }
            if stop.is_stop_requested() {
                drop(state);
                self.abandon_at(Instant::now());
                return SlotWait::Stopped;
            }
            let now = Instant::now();
            if now >= deadline {
                drop(state);
                self.abandon_at(now);
                return SlotWait::TimedOut;
            }
            let tick = RENDER_WAIT_TICK.min(deadline.saturating_duration_since(now));
            let (guard, _) = self
                .ready
                .wait_timeout(state, tick)
                .unwrap_or_else(|e| e.into_inner());
            state = guard;
        }
    }
}

/// 未完了のまま解放された受け皿を在庫から落とす。
///
/// 計上は受け皿の生成時に行うため、完了・放棄・投入失敗のどれも通らずに
/// 解放されると未完了の件数が 1 残る。残ると [`RenderInventory::outstanding`]
/// が二度と 0 にならず、**終了のたびに全タスクの完了待ちを呼ぶ**ようになる。
/// 費用も危険も無いはずの既定の経路が失われる。
///
/// 取り下げが二重に走らないのは、状態が `Pending` のときだけ落とすためである。
/// 完了・放棄・投入失敗はいずれも `Pending` から出るため、ここには来ない。
impl Drop for RenderSlot {
    fn drop(&mut self) {
        // 解放の時点で他に所有者は居ないため、毒されていても中身は有効である。
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        if let SlotState::Pending = state {
            self.inventory.release_issue();
        }
    }
}

/// 完了コールバックの本体。
///
/// SDK の型に依存しない引数だけを取る。行うのは放棄済みかどうかの確認、
/// 寸法と長さの検証、詰め物を除いた複製、受け皿への格納だけである。符号化・
/// ファイル入出力・ログ出力は行わない。これらはホストのレンダリング用スレッドを
/// 占有し、失敗の経路も増やす。
pub fn deliver_frame(
    slot: &RenderSlot,
    requested_frame: u32,
    frame: u32,
    width: u32,
    height: u32,
    pitch: u32,
    buffer: &[u8],
) {
    if slot.note_callback_arrival() {
        return;
    }
    let result = buffer::extract(requested_frame, frame, width, height, pitch, buffer)
        .map(|extracted| RenderedFrame {
            frame,
            width: extracted.width,
            height: extracted.height,
            pixels: extracted.pixels,
        })
        .map_err(|rule| RenderError::InvalidBuffer { rule });
    slot.complete(result);
}

/// 完了コールバックの本体を panic から隔離して実行する。
///
/// SDK ラッパーのレンダリング用トランポリンは我々のクロージャを保護せず、
/// panic は C 境界で処理を打ち切る。ホストのプロセスごと落ちるため、
/// 境界の内側で捕捉する。
///
/// 捕捉したら受け皿を panic の結果で埋める。ここで埋めなければ要求元は期限まで
/// 待ってから期限超過を返す。原因を隠したうえに待ち時間を丸ごと無駄にする。
///
/// `body` を引数に取るのは、コールバックが行うことを増やしても隔離の口を
/// 1 つに保つためである。
pub fn guard_callback(slot: &RenderSlot, body: impl FnOnce()) {
    if std::panic::catch_unwind(AssertUnwindSafe(body)).is_err() {
        slot.complete(Err(RenderError::Panicked));
    }
}

/// [`deliver_frame`] を [`guard_callback`] で包んだもの。
pub fn deliver_frame_guarded(
    slot: &RenderSlot,
    requested_frame: u32,
    frame: u32,
    width: u32,
    height: u32,
    pitch: u32,
    buffer: &[u8],
) {
    guard_callback(slot, || {
        deliver_frame(slot, requested_frame, frame, width, height, pitch, buffer);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::error::BufferRule;
    use crate::test_support::with_silent_panic_hook;

    /// 幅 2・高さ 2・詰め物なしの整合した画像。
    fn sample_buffer() -> Vec<u8> {
        (0..16u8).collect()
    }

    fn deliver_sample(slot: &RenderSlot, frame: u32) {
        deliver_frame(slot, frame, frame, 2, 2, 8, &sample_buffer());
    }

    fn never_stop() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn a_delivered_frame_is_handed_to_the_waiter() {
        let inventory = RenderInventory::new();
        let slot = RenderSlot::new(inventory.clone());
        assert_eq!(inventory.outstanding(), 1);

        deliver_sample(&slot, 7);
        assert_eq!(inventory.outstanding(), 0, "完了で在庫から落ちる");

        let waited = slot.wait(Instant::now() + Duration::from_secs(5), &never_stop());
        let frame = match waited {
            SlotWait::Done(Ok(frame)) => frame,
            other => panic!("完了が回収できません: {other:?}"),
        };
        assert_eq!(frame.frame, 7);
        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 2);
        assert_eq!(frame.pixels, sample_buffer());
    }

    #[test]
    fn a_broken_buffer_is_delivered_as_a_typed_failure() {
        let slot = RenderSlot::new(RenderInventory::new());
        // 要求と違うフレームを返す。規則 1 の破れである。
        deliver_frame(&slot, 7, 8, 2, 2, 8, &sample_buffer());

        match slot.wait(Instant::now() + Duration::from_secs(5), &never_stop()) {
            SlotWait::Done(Err(RenderError::InvalidBuffer {
                rule: BufferRule::FrameMismatch,
            })) => {}
            other => panic!("規則の破れが伝わりません: {other:?}"),
        }
    }

    #[test]
    fn a_wait_without_a_callback_always_leaves_on_the_deadline() {
        let inventory = RenderInventory::new();
        let slot = RenderSlot::new(inventory.clone());

        let started = Instant::now();
        let waited = slot.wait(started + Duration::from_millis(150), &never_stop());
        assert!(matches!(waited, SlotWait::TimedOut), "{waited:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "期限を大きく超えて待ち続けました: {}ms",
            started.elapsed().as_millis()
        );
        assert!(slot.is_abandoned(), "期限超過で放棄されていません");
        assert_eq!(
            inventory.outstanding(),
            1,
            "放棄しても在庫からは消えない（ホスト側のタスクは生きている）"
        );
        assert_eq!(inventory.abandoned_within_ttl(Instant::now()), 1);
    }

    #[test]
    fn a_stop_request_leaves_the_wait_early() {
        let slot = RenderSlot::new(RenderInventory::new());
        let stop = AtomicBool::new(true);

        let started = Instant::now();
        let waited = slot.wait(started + Duration::from_secs(30), &stop);
        assert!(matches!(waited, SlotWait::Stopped), "{waited:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "停止要求を観測できていません: {}ms",
            started.elapsed().as_millis()
        );
        assert!(slot.is_abandoned(), "停止要求で放棄されていません");
    }

    #[test]
    fn a_late_callback_on_an_abandoned_slot_drops_the_image() {
        let inventory = RenderInventory::new();
        let slot = RenderSlot::new(inventory.clone());
        let _ = slot.wait(Instant::now(), &never_stop());
        assert!(slot.is_abandoned());

        deliver_sample(&slot, 7);

        assert!(slot.is_abandoned(), "放棄は終端であり遷移しない");
        assert_eq!(
            inventory.outstanding(),
            0,
            "遅れて届いたコールバックが在庫を減らす"
        );
        assert!(
            !matches!(*slot.lock_state(), SlotState::Done(_)),
            "放棄済みの受け皿が画像を抱えています"
        );
    }

    #[test]
    fn a_second_callback_does_not_overwrite_the_first_result() {
        let inventory = RenderInventory::new();
        let slot = RenderSlot::new(inventory.clone());

        deliver_sample(&slot, 7);
        // 2 度目は要求と違うフレームを返す。上書きされれば失敗が観測される。
        deliver_frame(&slot, 7, 8, 2, 2, 8, &sample_buffer());

        match slot.wait(Instant::now() + Duration::from_secs(5), &never_stop()) {
            SlotWait::Done(Ok(frame)) => assert_eq!(frame.frame, 7),
            other => panic!("最初の結果が失われました: {other:?}"),
        }
        assert_eq!(inventory.outstanding(), 0, "在庫が二重に減っていない");
    }

    #[test]
    fn a_second_callback_on_an_abandoned_slot_does_not_reduce_the_inventory_twice() {
        let inventory = RenderInventory::new();
        let first = RenderSlot::new(inventory.clone());
        let second = RenderSlot::new(inventory.clone());
        let _ = first.wait(Instant::now(), &never_stop());
        let _ = second.wait(Instant::now(), &never_stop());
        assert_eq!(inventory.abandoned_within_ttl(Instant::now()), 2);

        deliver_sample(&first, 7);
        deliver_sample(&first, 7);

        assert_eq!(
            inventory.abandoned_within_ttl(Instant::now()),
            1,
            "同じ受け皿への 2 度目の到着で計上が二重に減りました"
        );
    }

    #[test]
    fn an_abandoned_slot_is_never_invisible_to_the_shutdown_count() {
        // 放棄は放棄済みへの計上と未完了からの取り下げの 2 段で進む。取り下げを
        // 先に行うと、2 段の隙間で在庫が 0 に見える。終了判定がその瞬間を読むと、
        // ホスト側でタスクが生きているのに待たずにアンロードへ進む。
        //
        // 放棄済みの記録を外から押さえておくと、放棄は計上の段で必ず止まる。
        // その時点で未完了の件数がまだ 1 であることが、順序そのものを表す。
        let inventory = RenderInventory::new();
        let slot = RenderSlot::new(inventory.clone());
        let blocked = inventory.abandoned.lock().unwrap();

        let abandoning = {
            let slot = slot.clone();
            std::thread::spawn(move || slot.abandon_at(Instant::now()))
        };
        std::thread::sleep(Duration::from_millis(50));

        assert_eq!(
            inventory.pending.load(Ordering::Acquire),
            1,
            "放棄済みへ計上する前に未完了から取り下げています"
        );

        drop(blocked);
        abandoning.join().unwrap();
        assert_eq!(inventory.outstanding(), 1);
    }

    #[test]
    fn a_slot_dropped_while_pending_gives_its_count_back() {
        // 計上は受け皿の生成時に行う。完了も放棄も通らずに解放されると、
        // 未完了の件数が残り続けて終了判定が二度と 0 にならない。
        let inventory = RenderInventory::new();
        drop(RenderSlot::new(inventory.clone()));
        assert_eq!(inventory.outstanding(), 0);
    }

    #[test]
    fn dropping_a_settled_slot_does_not_give_the_count_back_twice() {
        // 取り下げが二重に走ると、まだ生きている別のタスクの分まで消える。
        // 引き算は飽和するため、ずれても表には出ない。
        let inventory = RenderInventory::new();
        let alive = RenderSlot::new(inventory.clone());
        let settled = RenderSlot::new(inventory.clone());
        deliver_sample(&settled, 7);
        assert_eq!(inventory.outstanding(), 1);

        drop(settled);
        assert_eq!(
            inventory.outstanding(),
            1,
            "完了済みの受け皿の解放が、生きているタスクの計上まで消しました"
        );

        let abandoned = RenderSlot::new(inventory.clone());
        abandoned.abandon_at(Instant::now());
        assert_eq!(inventory.outstanding(), 2);
        drop(abandoned);
        assert_eq!(
            inventory.outstanding(),
            2,
            "放棄済みの受け皿の解放が計上を消しました"
        );

        drop(alive);
    }

    #[test]
    fn a_failed_issue_is_not_counted_as_abandoned() {
        let inventory = RenderInventory::new();
        let slot = RenderSlot::new(inventory.clone());
        assert_eq!(inventory.outstanding(), 1);

        slot.cancel_unissued();

        assert_eq!(inventory.outstanding(), 0);
        assert_eq!(inventory.abandoned_within_ttl(Instant::now()), 0);
        assert!(inventory.accepts_new_render(Instant::now()));
    }

    #[test]
    fn admission_stops_at_the_abandoned_cap_and_recovers_on_arrival() {
        let inventory = RenderInventory::new();
        let now = Instant::now();
        let mut slots = Vec::new();
        for _ in 0..MAX_ABANDONED_RENDERS {
            let slot = RenderSlot::new(inventory.clone());
            slot.abandon_at(now);
            slots.push(slot);
        }

        assert!(
            !inventory.accepts_new_render(now),
            "上限に達しても受け付けています"
        );
        assert_eq!(inventory.outstanding(), MAX_ABANDONED_RENDERS);

        // 忘れたタスクのコールバックが遅れて届く。
        deliver_sample(&slots[0], 7);

        assert!(
            inventory.accepts_new_render(now),
            "到着で計上が減らず、受付が戻りません"
        );
        assert_eq!(inventory.outstanding(), MAX_ABANDONED_RENDERS - 1);
    }

    #[test]
    fn admission_recovers_even_when_expired_entries_are_mixed_in() {
        // 期限切れの記録は受付判定に数えられていない。到着でそれを落としても
        // 受付は戻らない。落とすべきは TTL 内の記録である。
        let inventory = RenderInventory::new();
        let long_ago = Instant::now();
        for _ in 0..MAX_ABANDONED_RENDERS {
            RenderSlot::new(inventory.clone()).abandon_at(long_ago);
        }

        let recently = long_ago + ABANDONED_ENTRY_TTL + Duration::from_secs(10);
        let fresh: Vec<_> = (0..MAX_ABANDONED_RENDERS)
            .map(|_| {
                let slot = RenderSlot::new(inventory.clone());
                slot.abandon_at(recently);
                slot
            })
            .collect();

        assert_eq!(
            inventory.abandoned_within_ttl(recently),
            MAX_ABANDONED_RENDERS
        );
        assert!(!inventory.accepts_new_render(recently));

        // 新しく放棄したタスクのコールバックが遅れて届く。
        deliver_sample(&fresh[0], 7);

        assert_eq!(
            inventory.abandoned_within_ttl(recently),
            MAX_ABANDONED_RENDERS - 1,
            "到着で減ったのが期限切れの記録だけになっています"
        );
        assert!(
            inventory.accepts_new_render(recently),
            "期限切れの記録が先頭にあると受付が戻りません"
        );
        assert_eq!(
            inventory.outstanding(),
            MAX_ABANDONED_RENDERS * 2 - 1,
            "終了判定の件数は 1 件だけ減る"
        );
    }

    #[test]
    fn admission_also_recovers_once_the_entries_are_old_enough() {
        // コールバックが永久に来ない場合でも受付は戻る。戻らなければ、
        // 数回の期限超過でレンダリングがプロセスの寿命の間ずっと使えなくなる。
        let inventory = RenderInventory::new();
        let abandoned_at = Instant::now();
        for _ in 0..MAX_ABANDONED_RENDERS {
            let slot = RenderSlot::new(inventory.clone());
            slot.abandon_at(abandoned_at);
        }
        assert!(!inventory.accepts_new_render(abandoned_at));

        let later = abandoned_at + ABANDONED_ENTRY_TTL;
        assert!(
            inventory.accepts_new_render(later),
            "時間が経っても受付が戻りません"
        );
        assert_eq!(
            inventory.outstanding(),
            MAX_ABANDONED_RENDERS,
            "終了判定は時間で減衰させない"
        );
    }

    #[test]
    fn a_panicking_callback_fills_the_slot_instead_of_making_the_waiter_wait() {
        let inventory = RenderInventory::new();
        let slot = RenderSlot::new(inventory.clone());

        with_silent_panic_hook(|| {
            guard_callback(&slot, || {
                panic!("レンダリング結果の受け渡しで panic が起きました")
            });
        });

        match slot.wait(Instant::now() + Duration::from_secs(5), &never_stop()) {
            SlotWait::Done(Err(RenderError::Panicked)) => {}
            other => panic!("panic が受け皿へ伝わりません: {other:?}"),
        }
        assert_eq!(inventory.outstanding(), 0);
    }

    #[test]
    fn a_panic_after_a_delivered_frame_does_not_replace_the_result() {
        let slot = RenderSlot::new(RenderInventory::new());
        with_silent_panic_hook(|| {
            guard_callback(&slot, || {
                deliver_sample(&slot, 7);
                panic!("結果を置いた後で panic する");
            });
        });

        match slot.wait(Instant::now() + Duration::from_secs(5), &never_stop()) {
            SlotWait::Done(Ok(frame)) => assert_eq!(frame.frame, 7),
            other => panic!("置き終えた結果が panic で失われました: {other:?}"),
        }
    }

    #[test]
    fn the_guarded_entry_point_delivers_a_normal_frame_unchanged() {
        let slot = RenderSlot::new(RenderInventory::new());
        deliver_frame_guarded(&slot, 7, 7, 2, 2, 8, &sample_buffer());
        match slot.wait(Instant::now() + Duration::from_secs(5), &never_stop()) {
            SlotWait::Done(Ok(frame)) => assert_eq!(frame.pixels, sample_buffer()),
            other => panic!("隔離した経路で結果が失われました: {other:?}"),
        }
    }

    #[test]
    fn a_callback_from_another_thread_wakes_the_waiter() {
        let inventory = RenderInventory::new();
        let slot = RenderSlot::new(inventory.clone());
        let callback_slot = slot.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            deliver_sample(&callback_slot, 7);
        });

        let waited = slot.wait(Instant::now() + Duration::from_secs(5), &never_stop());
        assert!(matches!(waited, SlotWait::Done(Ok(_))), "{waited:?}");
        handle.join().unwrap();
        assert_eq!(inventory.outstanding(), 0);
    }

    #[test]
    fn the_wait_can_observe_the_accept_loop_stop_itself() {
        // 待機が見る停止は、接続受理ループを止めるものと同一でなければならない。
        // 別の合図を用意すると、片方が立ってからもう片方が立つまでの間、待機が
        // 居座って停止手順を待たせる。
        //
        // 確かめるのは「そのまま渡せること」である。合図を新たに作らせない
        // ために作成と送出は借りていないため、実際に立てて確かめるのは接続
        // 受理ループ側のテストの担当になる。ここで固定するのは、次の配線が
        // 2 つ目のフラグを作らずに済む形が保たれていることである。
        fn assert_observable<T: StopRequest + ?Sized>() {}
        assert_observable::<crate::pipe::StopSignal>();
        assert_observable::<dyn StopRequest>();
    }

    #[test]
    fn neither_a_frame_nor_a_wait_outcome_prints_its_pixels() {
        // 画像には利用者のプロジェクトの内容が写る。表示する場所を後から全て
        // 見張るより、型の側で出せなくしておく。
        let frame = RenderedFrame {
            frame: 7,
            width: 2,
            height: 2,
            pixels: sample_buffer(),
        };
        let shown = format!("{frame:?}");
        assert!(shown.contains("pixel_bytes: 16"), "{shown}");
        assert!(!shown.contains("pixels"), "{shown}");

        let outcome = SlotWait::Done(Ok(frame));
        let shown = format!("{outcome:?}");
        assert!(shown.contains("pixel_bytes: 16"), "{shown}");
        assert!(!shown.contains("pixels"), "{shown}");
    }

    #[test]
    fn a_poisoned_state_does_not_block_later_use() {
        // コールバックはホストのレンダリング用スレッドで走る。状態のロックを
        // 毒したままにすると、以後そこへ触れる全ての経路が panic する。
        let slot = RenderSlot::new(RenderInventory::new());
        let poisoning = Arc::clone(&slot);
        with_silent_panic_hook(|| {
            let _ = std::thread::spawn(move || {
                let _guard = poisoning.state.lock().unwrap();
                panic!("状態を保持したまま panic する");
            })
            .join();
        });

        deliver_sample(&slot, 7);
        assert!(matches!(
            slot.wait(Instant::now() + Duration::from_secs(5), &never_stop()),
            SlotWait::Done(Ok(_))
        ));
    }
}
