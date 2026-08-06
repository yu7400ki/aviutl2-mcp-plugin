//! 編集情報とシーンの読み取り DTO。
//!
//! frame 番号・layer 番号はいずれも 0 始まりである。

use crate::number::FiniteF64;
use serde::{Deserialize, Serialize};

/// 現在の編集情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditInfo {
    /// 現在シーンの情報。
    pub scene: SceneInfo,
    /// 編集カーソル位置。
    pub cursor: Cursor,
    /// オブジェクトが存在する範囲。
    pub extent: Extent,
    /// タイムライン表示範囲。
    pub display: DisplayRange,
    /// 選択範囲。未選択は null。
    pub selected_range: Option<FrameRange>,
    /// グリッド BPM の一覧。
    pub grid_bpm: Vec<GridBpm>,
    /// プロジェクトの epoch。
    pub project_epoch: String,
    /// プロジェクトの revision。
    pub project_revision: u64,
}

/// BPM グリッドの 1 件。
///
/// 読み取りと書き込みで同じ形を用いる。読み取った一覧をそのまま書き戻せることが
/// この型の目的であり、一部のフィールドだけを運ぶ形にすると、差し替えたい要素
/// 以外の値が書き戻しで失われる。
///
/// **未知フィールドを拒否しない。** [`ObjectSelector`](crate::selector::ObjectSelector)
/// と同じく、応答に含めて返し次の要求でそのまま送り返す双方向の型である。応答を
/// 組み立てる側と要求を復号する側は別のプロセスであり、版が揃うとは限らない。
/// 新しい側が足した field を古い側が拒むと、応答をそのまま送り返す往復が壊れる。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GridBpm {
    /// テンポ。
    pub tempo: FiniteF64,
    /// 拍子。
    ///
    /// SDK は 32bit 符号付き整数で受け渡す。収まらない値を要求として受け取って
    /// から拒否できるよう、DTO はより広い整数で持つ。
    pub beat: i64,
    /// 開始位置（秒）。
    pub start: FiniteF64,
    /// 拍子オフセット（秒）。
    pub offset: FiniteF64,
}

/// シーンの詳細情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneInfo {
    /// シーン ID。
    pub id: i32,
    /// シーン名。取得不能時は null。
    pub name: Option<String>,
    /// 画像の横幅。
    pub width: u32,
    /// 画像の高さ。
    pub height: u32,
    /// フレームレート。`fps_scale` が 0 のときは算出できないため null。
    pub fps: Option<FiniteF64>,
    /// フレームレートの分子。約分された値であり、編集情報が持つ元の分子とは
    /// 限らない（60000/1000 は 60/1 として現れる）。
    pub fps_rate: i32,
    /// フレームレートの分母。`fps_rate` と同じく約分された値。
    pub fps_scale: i32,
    /// 音声のサンプリングレート。
    pub sample_rate: u32,
}

/// 編集カーソル位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// 0 始まりのフレーム番号。
    pub frame: usize,
    /// 0 始まりのレイヤー番号。
    pub layer: usize,
}

/// オブジェクトが存在する範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extent {
    /// オブジェクトが存在する最大フレーム番号。
    pub frame_max: usize,
    /// オブジェクトが存在する最大レイヤー番号。
    pub layer_max: usize,
}

/// タイムラインの表示範囲。
///
/// `frame_num` / `layer_num` は表示上の概数であり、厳密な件数ではない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayRange {
    /// 表示開始フレーム番号。
    pub frame_start: usize,
    /// 表示開始レイヤー番号。
    pub layer_start: usize,
    /// 表示フレーム数。
    pub frame_num: usize,
    /// 表示レイヤー数。
    pub layer_num: usize,
}

/// フレーム範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRange {
    /// 0 始まりの開始フレーム番号。
    pub start: usize,
    /// 0 始まりの終了フレーム番号。
    pub end: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_scene_info() -> SceneInfo {
        SceneInfo {
            id: 0,
            name: Some("Scene 1".to_string()),
            width: 1920,
            height: 1080,
            fps: FiniteF64::try_new(30000.0 / 1001.0),
            fps_rate: 30000,
            fps_scale: 1001,
            sample_rate: 48000,
        }
    }

    fn sample_edit_info() -> EditInfo {
        EditInfo {
            scene: sample_scene_info(),
            cursor: Cursor {
                frame: 120,
                layer: 2,
            },
            extent: Extent {
                frame_max: 3600,
                layer_max: 15,
            },
            display: DisplayRange {
                frame_start: 0,
                layer_start: 0,
                frame_num: 600,
                layer_num: 10,
            },
            selected_range: Some(FrameRange { start: 10, end: 20 }),
            grid_bpm: vec![GridBpm {
                tempo: FiniteF64::try_new(120.0).unwrap(),
                beat: 4,
                start: FiniteF64::try_new(0.0).unwrap(),
                offset: FiniteF64::try_new(0.25).unwrap(),
            }],
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 42,
        }
    }

    #[test]
    fn edit_info_roundtrip() {
        let info = sample_edit_info();
        let s = serde_json::to_string(&info).unwrap();
        let restored: EditInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, info);
    }

    #[test]
    fn edit_info_allows_unknown_optional_fields() {
        let mut value = serde_json::to_value(sample_edit_info()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::json!(1));
        let restored: EditInfo = serde_json::from_value(value).unwrap();
        assert_eq!(restored, sample_edit_info());
    }

    #[test]
    fn grid_bpm_carries_four_fields() {
        // tempo だけを運ぶ形へ戻すと、読み取った一覧をそのまま書き戻す経路で
        // 残りの 3 つが失われる。
        let value = serde_json::to_value(sample_edit_info()).unwrap();
        let entry = &value["grid_bpm"][0];
        assert_eq!(entry["tempo"], serde_json::json!(120.0));
        assert_eq!(entry["beat"], serde_json::json!(4));
        assert_eq!(entry["start"], serde_json::json!(0.0));
        assert_eq!(entry["offset"], serde_json::json!(0.25));
    }

    #[test]
    fn grid_bpm_allows_unknown_optional_fields() {
        // 応答に含めて返し、次の要求でそのまま送り返す双方向の型である。応答を
        // 組み立てる側と要求を復号する側で版が揃うとは限らないため、未知
        // フィールドを拒否しない。
        let mut value = serde_json::to_value(sample_edit_info().grid_bpm[0]).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::json!(1));
        let restored: GridBpm = serde_json::from_value(value).unwrap();
        assert_eq!(restored, sample_edit_info().grid_bpm[0]);
    }

    #[test]
    fn scene_info_keeps_raw_fps_when_unavailable() {
        // 分母 0 では fps を算出できないが、生の rate/scale は保持する。
        let scene = SceneInfo {
            fps: None,
            fps_rate: 30000,
            fps_scale: 0,
            ..sample_scene_info()
        };
        let s = serde_json::to_string(&scene).unwrap();
        let restored: SceneInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(restored.fps, None);
        assert_eq!(restored.fps_rate, 30000);
        assert_eq!(restored.fps_scale, 0);
    }

    #[test]
    fn scene_info_roundtrip() {
        let scene = sample_scene_info();
        let s = serde_json::to_string(&scene).unwrap();
        let restored: SceneInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, scene);
    }

    #[test]
    fn selected_range_unselected_is_null() {
        let info = EditInfo {
            selected_range: None,
            ..sample_edit_info()
        };
        // SDK が未選択に使う -1 ではなく null として表す。
        let value = serde_json::to_value(&info).unwrap();
        assert_eq!(value["selected_range"], serde_json::Value::Null);

        let restored: EditInfo = serde_json::from_value(value).unwrap();
        assert_eq!(restored.selected_range, None);
    }

    #[test]
    fn frame_range_roundtrip() {
        let range = FrameRange { start: 0, end: 99 };
        let s = serde_json::to_string(&range).unwrap();
        let restored: FrameRange = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, range);
    }
}
