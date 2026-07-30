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
//! 必要な編集情報は区間の外で取る。
//!
//! # 構成
//!
//! | モジュール | 担当 |
//! |---|---|
//! | [`error`] | 失敗の分類と、応答へ載せる安全な補助情報 |
//! | [`buffer`] | ホストが渡す pixel buffer の検証と詰め直し |
//! | [`slot`] | 完了コールバックと要求元が共有する受け皿、投入したタスクの在庫 |
//! | [`handoff`] | 成果物の符号化・原子的な書き出し・掃除 |

pub mod buffer;
pub mod error;
pub mod handoff;
pub mod slot;

pub use buffer::{ExtractedFrame, FrameLayout, MAX_RENDER_FRAME_BYTES};
pub use error::{ArtifactStage, BufferRule, RenderError, RenderStage};
pub use handoff::{ARTIFACT_MEDIA_TYPE, HANDOFF_TTL, HandoffArtifact, HandoffDir, HandoffToken};
pub use slot::{
    ABANDONED_ENTRY_TTL, MAX_ABANDONED_RENDERS, RENDER_WAIT_TICK, RenderInventory, RenderSlot,
    RenderedFrame, SlotWait, deliver_frame, deliver_frame_guarded, guard_callback,
};
