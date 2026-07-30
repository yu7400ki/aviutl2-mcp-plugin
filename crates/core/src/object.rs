//! レイヤーとオブジェクトの読み取り DTO。
//!
//! frame 番号・layer 番号はいずれも 0 始まりである。

use crate::effect::EffectInfo;
use crate::fingerprint::{Fingerprint, FingerprintAlgorithm, ObjectFingerprintInput};
use crate::selector::ObjectSelector;
use serde::{Deserialize, Serialize};

/// 現在シーンのレイヤー概要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerInfo {
    /// 0 始まりのレイヤー番号。
    pub index: usize,
    /// レイヤー名。無名は null。
    pub name: Option<String>,
    /// レイヤーが有効か。
    pub enabled: bool,
    /// レイヤーがロックされているか。
    pub locked: bool,
    /// このレイヤーに存在するオブジェクト数。
    pub object_count: usize,
}

/// オブジェクトの概要。
///
/// トップレベルとセレクターの fingerprint は同一でなければならない。
/// [`ObjectSummary::new`] を用いると 1 度の算出結果が両方へ設定される。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectSummary {
    /// 0 始まりのレイヤー番号。
    pub layer: usize,
    /// 0 始まりの開始フレーム番号。
    pub frame_start: usize,
    /// 0 始まりの終了フレーム番号。
    pub frame_end: usize,
    /// オブジェクト名。標準名のままなら null。
    pub name: Option<String>,
    /// 再指定用のセレクター。
    pub selector: ObjectSelector,
    /// 同一性検証用の fingerprint。
    pub fingerprint: Fingerprint,
}

impl ObjectSummary {
    /// 概要とセレクターを、単一の fingerprint 算出結果から組み立てる。
    pub fn new(project_epoch: impl Into<String>, input: ObjectFingerprintInput<'_>) -> Self {
        let fingerprint = crate::fingerprint::object_fingerprint(input);
        Self {
            layer: input.layer,
            frame_start: input.frame_start,
            frame_end: input.frame_end,
            name: input.name.map(str::to_string),
            selector: ObjectSelector {
                project_epoch: project_epoch.into(),
                scene_id: input.scene_id,
                layer: input.layer,
                frame: input.frame_start,
                name: input.name.map(str::to_string),
                fingerprint: fingerprint.clone(),
                fingerprint_algorithm: Some(FingerprintAlgorithm::GENERATED),
            },
            fingerprint,
        }
    }
}

/// オブジェクトの詳細。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectDetail {
    /// 概要。
    pub summary: ObjectSummary,
    /// 正規化前の alias。
    pub alias: String,
    /// 中間点で分割された区間。
    pub sections: Vec<SectionRange>,
    /// 付与された effect の列。
    pub effects: Vec<EffectInfo>,
    /// 取得時点のプロジェクト revision。
    pub project_revision: u64,
}

/// 中間点で区切られた区間。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionRange {
    /// 0 始まりの開始フレーム番号。
    pub start: usize,
    /// 0 始まりの終了フレーム番号。
    pub end: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_input() -> ObjectFingerprintInput<'static> {
        ObjectFingerprintInput {
            scene_id: 0,
            layer: 2,
            frame_start: 120,
            frame_end: 240,
            name: Some("立ち絵"),
            alias: "alias",
        }
    }

    fn sample_object_summary() -> ObjectSummary {
        ObjectSummary::new("78be92d1-c8c9-44c6-ae52-387548971468", sample_input())
    }

    fn sample_object_detail() -> ObjectDetail {
        ObjectDetail {
            summary: sample_object_summary(),
            alias: "[vo]\n_name=立ち絵\n".to_string(),
            sections: vec![
                SectionRange {
                    start: 120,
                    end: 180,
                },
                SectionRange {
                    start: 180,
                    end: 240,
                },
            ],
            effects: Vec::new(),
            project_revision: 42,
        }
    }

    #[test]
    fn layer_info_roundtrip() {
        let layer = LayerInfo {
            index: 0,
            name: None,
            enabled: true,
            locked: false,
            object_count: 3,
        };
        let s = serde_json::to_string(&layer).unwrap();
        let restored: LayerInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, layer);
    }

    #[test]
    fn layer_info_allows_unknown_optional_fields() {
        let s =
            r#"{"index":0,"name":null,"enabled":true,"locked":false,"object_count":0,"future":1}"#;
        let layer: LayerInfo = serde_json::from_str(s).unwrap();
        assert_eq!(layer.index, 0);
    }

    #[test]
    fn object_summary_roundtrip() {
        let summary = sample_object_summary();
        let s = serde_json::to_string(&summary).unwrap();
        let restored: ObjectSummary = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, summary);
    }

    #[test]
    fn object_summary_standard_name_is_null() {
        let summary = ObjectSummary {
            name: None,
            ..sample_object_summary()
        };
        let value = serde_json::to_value(&summary).unwrap();
        assert_eq!(value["name"], serde_json::Value::Null);
        let restored: ObjectSummary = serde_json::from_value(value).unwrap();
        assert_eq!(restored.name, None);
    }

    #[test]
    fn object_detail_roundtrip() {
        let detail = sample_object_detail();
        let s = serde_json::to_string(&detail).unwrap();
        let restored: ObjectDetail = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, detail);
    }

    #[test]
    fn object_detail_allows_unknown_optional_fields() {
        let mut value = serde_json::to_value(sample_object_detail()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::json!(1));
        let restored: ObjectDetail = serde_json::from_value(value).unwrap();
        assert_eq!(restored, sample_object_detail());
    }

    #[test]
    fn object_summary_shares_one_fingerprint_with_selector() {
        let summary = sample_object_summary();
        assert_eq!(summary.fingerprint, summary.selector.fingerprint);
    }

    #[test]
    fn object_summary_does_not_report_a_fingerprint_algorithm() {
        // 方式は digest の材料であって運ぶ値ではない。
        let value = serde_json::to_value(sample_object_summary()).unwrap();
        assert!(
            value.get("fingerprint_algorithm").is_none(),
            "{value} が算出方式を返しています"
        );
    }

    #[test]
    fn object_summary_new_copies_input_into_selector() {
        let summary = sample_object_summary();
        assert_eq!(summary.selector.scene_id, sample_input().scene_id);
        assert_eq!(summary.selector.layer, summary.layer);
        // セレクターは開始フレームの完全一致で対象を照合する。
        assert_eq!(summary.selector.frame, summary.frame_start);
        assert_eq!(summary.selector.name, summary.name);
        assert_eq!(
            summary.selector.project_epoch,
            "78be92d1-c8c9-44c6-ae52-387548971468"
        );
    }

    #[test]
    fn section_range_roundtrip() {
        let section = SectionRange { start: 0, end: 10 };
        let s = serde_json::to_string(&section).unwrap();
        let restored: SectionRange = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, section);
    }
}
