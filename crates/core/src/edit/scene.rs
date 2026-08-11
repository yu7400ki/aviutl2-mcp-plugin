//! シーン設定と BPM グリッドの params と result。

use super::{
    EditInputError, FIELD_NAME, FIELD_SAMPLE_RATE, FIELD_SIZE, FIELD_SIZE_HEIGHT, FIELD_SIZE_WIDTH,
    validate_grid_bpm_entries, validate_scene_value,
};
use crate::edit_info::{GridBpm, SceneInfo};
use crate::render::MAX_RENDER_FRAME_BYTES;
use crate::validation::{TextSyntaxError, validate_name};
use serde::{Deserialize, Serialize};

/// `set_grid_bpm` の params。
///
/// BPM グリッドはシーンに属し、対象を指す selector を持たない。守れるのは
/// プロジェクト境界と現在シーンだけであり、「読み取った時点と同じ一覧か」は
/// 確かめられない。応答は read-back で得た実際の一覧を返すため、要求元は
/// それを見て判断する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetGridBpmParams {
    /// 現在シーンの一致確認に使う guard。
    pub expected_scene_id: i32,
    /// 置き換える BPM 情報の一覧。
    ///
    /// 部分更新ではない。指定した一覧がそのまま現在の一覧になる。0 件は
    /// グリッドを消す指定であり、[`MAX_GRID_BPM_ENTRIES`](super::MAX_GRID_BPM_ENTRIES) 件までを受け付ける。
    pub entries: Vec<GridBpm>,
    /// 応答が返した `project_epoch`。
    ///
    /// BPM グリッドは selector を持たないため、プロジェクト境界を照合する
    /// 唯一の材料である。
    pub expected_project_epoch: String,
}

impl SetGridBpmParams {
    /// 要求内容だけで決まる検証を行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_grid_bpm_entries(&self.entries)
    }
}

/// シーンの解像度。
///
/// **横幅と高さを別々のフィールドへ平坦化しない。** ホストは解像度を 1 回の
/// 呼び出しで受け取り、片方だけを変える手段を持たない。平坦化すると「横幅だけ
/// 指定」が綴れてしまい、綴れるのに実現できない要求を受け付けることになる。
/// 組にしておけば、片方だけの指定は必須フィールドの欠落として復号の段で落ちる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSize {
    /// 画像の横幅。1 以上。
    pub width: u32,
    /// 画像の高さ。1 以上。
    pub height: u32,
}

impl SceneSize {
    /// 解像度が受け渡せる範囲と、1 フレームを描ける大きさに収まることを確認する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_scene_value(FIELD_SIZE_WIDTH, self.width)?;
        validate_scene_value(FIELD_SIZE_HEIGHT, self.height)?;
        // 上限は描画の側と共有する。描けない大きさのシーンを作れてしまうと、
        // 作った本人がそのシーンを 1 度も描けない。
        //
        // 積は必ず 64bit で取る。`u32` 同士の積は容易に溢れ、溢れた値は上限を
        // 下回るため、判定が通ってしまう。
        let frame_bytes = u64::from(self.width) * u64::from(self.height) * 4;
        if frame_bytes > MAX_RENDER_FRAME_BYTES {
            return Err(EditInputError::SceneFrameTooLarge {
                bytes: frame_bytes,
                max: MAX_RENDER_FRAME_BYTES,
            });
        }
        Ok(())
    }
}

/// `set_scene_settings` の params。
///
/// シーンは selector も fingerprint も持たない。守れるのはプロジェクト境界と
/// 現在シーンと値の範囲だけであり、「読み取った時点と同じ状態のシーンか」は
/// 確かめられない。応答は変更後に観測した実際の状態を返すため、要求元はそれを
/// 見て判断する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetSceneSettingsParams {
    /// 現在シーンの一致確認に使う guard。
    ///
    /// 変更は常に現在シーンへ掛かる。非現在シーンを操作する手段は無く、この値は
    /// 探索先ではなく guard である。
    pub expected_scene_id: i32,
    /// シーン名。省略時は変更しない。
    ///
    /// 空文字は受け付けない。ホストは空文字と未指定をどちらも「変更しない」と
    /// して無視するため、受け付ければ何も起きなかった要求を成功として返す
    /// ことになる。オブジェクト名やレイヤー名と違い、シーン名には「標準へ戻す」
    /// 意味も戻す先も無いため、取り消しを表す指定も持たない。
    #[serde(default)]
    pub name: Option<String>,
    /// 解像度。省略時は変更しない。
    #[serde(default)]
    pub size: Option<SceneSize>,
    /// 音声のサンプリングレート。省略時は変更しない。
    #[serde(default)]
    pub sample_rate: Option<u32>,
    /// 応答が返した `project_epoch`。
    ///
    /// シーンは selector を持たないため、プロジェクト境界を照合する唯一の
    /// 材料である。
    pub expected_project_epoch: String,
}

impl SetSceneSettingsParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 3 つ全ての省略は拒否する。何も変更しない編集要求は、成功したのか
    /// 無視されたのかをクライアントが区別できない。
    pub fn validate(&self) -> Result<(), EditInputError> {
        if self.name.is_none() && self.size.is_none() && self.sample_rate.is_none() {
            return Err(EditInputError::NoChangeRequested {
                fields: &[FIELD_NAME, FIELD_SIZE, FIELD_SAMPLE_RATE],
            });
        }
        if let Some(name) = &self.name {
            if name.is_empty() {
                return Err(EditInputError::Text {
                    field: FIELD_NAME,
                    source: TextSyntaxError::Empty,
                });
            }
            validate_name(name).map_err(|source| EditInputError::Text {
                field: FIELD_NAME,
                source,
            })?;
        }
        if let Some(size) = &self.size {
            size.validate()?;
        }
        if let Some(sample_rate) = self.sample_rate {
            // 受理値の一覧は作らない。SDK にも文書にも記述が無く、我々が列挙
            // すると、ホストが受け付ける値を我々の側で拒むことになる。
            validate_scene_value(FIELD_SAMPLE_RATE, sample_rate)?;
        }
        Ok(())
    }
}

/// BPM グリッドの一覧の置き換えの結果。
///
/// 一覧そのものが read-back であり、要求した値がどう正規化されたかはこの一覧が
/// 答える。
///
/// BPM グリッドはプロジェクトへ保存される内容であるため、この変更は revision を
/// 進める。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridBpmOutcome {
    /// 変更後のプロジェクトの epoch。
    pub project_epoch: String,
    /// 変更を反映したあとの revision。
    pub project_revision: u64,
    /// read-back で得た変更後の一覧。
    ///
    /// 要求した値と一致するとは限らない。ホストは単精度へ丸め、並べ替えもし得る。
    pub entries: Vec<GridBpm>,
}

/// シーン設定の変更の結果。
///
/// [`SceneInfo`] は読み取りの DTO をそのまま用いる。`get_current_scene` が返す
/// ものと同じ型であるため、要求元は読みと書きで別の形を覚えなくてよい。
///
/// シーンの名前・解像度・サンプリングレートはプロジェクトへ保存される内容で
/// あるため、この変更は revision を進める。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneSettingsOutcome {
    /// 変更後のプロジェクトの epoch。
    pub project_epoch: String,
    /// 変更を反映したあとの revision。
    pub project_revision: u64,
    /// 変更後に観測したシーンの状態。
    ///
    /// 要求した値と一致するとは限らない。ホストが値を調整し得るうえ、観測は
    /// 編集と原子的でない（[`Self::observed_after_edit`]）。差異そのものは
    /// 失敗ではない。
    pub scene: SceneInfo,
    /// 解像度とサンプリングレートが編集の区間の外で観測されたことを示す。
    ///
    /// 常に `true` である。反映値は編集情報にしか現れず、区間の内側から読み
    /// 直す手段が無いため、観測までの間に他所からの変更が入り得る。シーン名
    /// だけは区間の内側で照合済みである。
    ///
    /// **このフィールドを応答へ載せ続けるかは判断していない。** 常に同じ値を
    /// 返すうえ、同じことを tool の説明と text content も述べており、応答値として
    /// 持つ必要があるかは確かめていない。**判断していないことをここに書き残す**
    /// ——書かなければ、次に読む者は理由が在ると考えて探す。
    pub observed_after_edit: bool,
    /// この変更が取り消せないことを示す。
    ///
    /// 常に `true` である。AviUtl2 の取り消し操作ではシーン設定は元へ戻らず、
    /// 取り消すとその前に行った編集が取り消される。**成功したあとにも読める
    /// 唯一の口である** — tool の説明と annotation は要求を出す前にしか効かず、
    /// 応答だけを見る経路はそこから性質を拾えない。
    pub non_undoable: bool,
}
