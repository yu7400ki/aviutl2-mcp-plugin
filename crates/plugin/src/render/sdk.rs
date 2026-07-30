//! SDK のレンダリング API を [`RenderHost`] へ写す実装。
//!
//! ここだけがレンダリングの投入と全タスク待ちを呼ぶ。完了コールバックへ渡す
//! クロージャを組み立てるのもここ 1 か所であり、クロージャが捕捉するのは
//! 受け皿と要求したフレーム番号だけである。plugin の他の状態を捕捉すると、
//! ホストのレンダリング用スレッドからそこへ触れる経路が生まれる。
//!
//! 準備状態・編集状態・編集情報は読み取り経路の実装をそのまま用いる。同じ
//! ホストの同じ値を層ごとに別々に写すと、受付判定と読み取りが別の値を見る
//! ことになる。

use crate::EDIT_HANDLE;
use crate::read::host::{EditState, HostEditInfo, ReadHost};
use crate::read::sdk::SdkReadHost;
use crate::render::error::RenderError;
use crate::render::host::RenderHost;
use crate::render::slot::{RenderSlot, deliver_frame_guarded};
use std::sync::Arc;

/// グローバルな編集ハンドルを介して SDK を呼ぶホスト。
pub struct SdkRenderHost {
    read: SdkReadHost,
}

impl SdkRenderHost {
    /// SDK を呼ぶホストを作る。
    pub fn new() -> Self {
        Self { read: SdkReadHost }
    }
}

impl Default for SdkRenderHost {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderHost for SdkRenderHost {
    fn is_ready(&self) -> bool {
        self.read.is_ready()
    }

    fn edit_state(&self) -> Result<EditState, RenderError> {
        self.read.edit_state().map_err(RenderError::from)
    }

    fn edit_info(&self) -> Result<HostEditInfo, RenderError> {
        // 区間の外で取得する。この呼び出しはフレームレートの分母が 0 のとき
        // 落ちるため、呼び出し側が捕捉層で包むことを前提にしている。
        self.read.edit_info().map_err(RenderError::from)
    }

    fn request_scene_video(&self, frame: u32, slot: Arc<RenderSlot>) -> Result<(), RenderError> {
        // クロージャを保持する領域はコールバック自身が解放する。コールバックが
        // 一度も呼ばれなければ捕捉した値は残り続けるため、捕らえるのは受け皿と
        // 要求したフレーム番号だけに留める。
        EDIT_HANDLE
            .rendering_scene_video(frame, move |video| {
                deliver_frame_guarded(
                    &slot,
                    frame,
                    video.frame,
                    video.width,
                    video.height,
                    video.pitch,
                    video.buffer,
                );
            })
            .map_err(|_| RenderError::Sdk {
                operation: "rendering_scene_video",
            })
    }

    fn wait_all_tasks(&self) {
        // 準備前の呼び出しは失敗ではなく打ち切りで落ちる。一度でも投入していれば
        // 準備済みだが、投入していない場合にここへ来ても落とさない。
        if !EDIT_HANDLE.is_ready() {
            return;
        }
        EDIT_HANDLE.wait_rendering_task();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_uninitialized_edit_handle_is_not_ready() {
        // テストでは編集ハンドルが初期化されないため、準備前として扱われる。
        assert!(!SdkRenderHost::new().is_ready());
    }

    #[test]
    fn waiting_for_all_tasks_before_the_handle_is_ready_does_nothing() {
        // 全タスク待ちは準備前に呼ぶと打ち切りで落ちる。終了手順はハンドルの
        // 状態を選べないため、呼ばずに戻る経路が要る。
        SdkRenderHost::new().wait_all_tasks();
    }
}
