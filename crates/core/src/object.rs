//! レイヤーとオブジェクトの読み取り DTO。
//!
//! frame 番号・layer 番号はいずれも 0 始まりである。

use crate::effect::EffectInfo;
use crate::fingerprint::Fingerprint;
use crate::selector::ObjectSelector;
use serde::{Deserialize, Serialize};

/// 現在シーンのレイヤー概要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerInfo {
    /// 0 始まりのレイヤー番号。
    pub index: usize,
    /// レイヤー名。無名は null。
    pub name: Option<String>,
    pub enabled: bool,
    pub locked: bool,
    /// このレイヤーに存在するオブジェクト数。
    pub object_count: usize,
}

/// オブジェクトの概要。
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
    use crate::fingerprint::{FingerprintAlgorithm, object_fingerprint};

    fn sample_fingerprint() -> Fingerprint {
        object_fingerprint(
            &FingerprintAlgorithm::RawV1,
            0,
            2,
            120,
            240,
            Some("立ち絵"),
            "alias",
        )
    }

    fn sample_object_summary() -> ObjectSummary {
        ObjectSummary {
            layer: 2,
            frame_start: 120,
            frame_end: 240,
            name: Some("立ち絵".to_string()),
            selector: ObjectSelector {
                project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
                scene_id: 0,
                layer: 2,
                frame: 120,
                name: Some("立ち絵".to_string()),
                fingerprint: sample_fingerprint(),
                fingerprint_algorithm: FingerprintAlgorithm::RawV1,
            },
            fingerprint: sample_fingerprint(),
        }
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
    fn object_detail_hides_internal_identifiers() {
        let s = serde_json::to_string(&sample_object_detail()).unwrap();
        for forbidden in ["auth_secret", "handle", "pointer", "nonce"] {
            assert!(!s.contains(forbidden), "{forbidden} が直列化に現れている");
        }
    }

    #[test]
    fn section_range_roundtrip() {
        let section = SectionRange { start: 0, end: 10 };
        let s = serde_json::to_string(&section).unwrap();
        let restored: SectionRange = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, section);
    }
}
