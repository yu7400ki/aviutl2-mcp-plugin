//! opaque handle を公開せずに対象を再指定するセレクター。

use crate::fingerprint::{Fingerprint, FingerprintAlgorithm};
use serde::{Deserialize, Serialize};

/// オブジェクトを再指定するセレクター。
///
/// 応答に含めて返し、次の要求でそのまま送り返す双方向の型である。応答型の
/// 内側で前方互換を壊さないよう、未知フィールドを拒否しない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectSelector {
    /// 応答が返したプロジェクトの epoch。
    pub project_epoch: String,
    /// 読み取り時と同じシーンかを確認するための guard。
    pub scene_id: i32,
    /// 0 始まりのレイヤー番号。
    pub layer: usize,
    /// 0 始まりの開始フレーム番号。完全一致で照合する。
    pub frame: usize,
    /// オブジェクト名。指定時は一致を必須とする。
    pub name: Option<String>,
    /// 同一性検証用の fingerprint。
    pub fingerprint: Fingerprint,
    /// fingerprint の算出方式。再計算時に同じ方式を選ぶために持つ。
    pub fingerprint_algorithm: FingerprintAlgorithm,
}

/// オブジェクト内の effect を再指定するセレクター。
///
/// 同名 effect の順序は `effect_index` で表し、利用者へ名前の文字列結合を
/// 要求しない。[`ObjectSelector`] と同じ理由で未知フィールドを拒否しない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectSelector {
    /// effect が属するオブジェクト。
    pub object: ObjectSelector,
    /// effect 名。
    pub effect_name: String,
    /// 同名 effect のうち何番目か。0 始まり。
    pub effect_index: usize,
    /// 同一性検証用の fingerprint。
    pub fingerprint: Fingerprint,
    /// fingerprint の算出方式。
    pub fingerprint_algorithm: FingerprintAlgorithm,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::{ObjectFingerprintInput, object_fingerprint};

    fn sample_fingerprint(alias: &str) -> Fingerprint {
        object_fingerprint(ObjectFingerprintInput {
            scene_id: 0,
            layer: 2,
            frame_start: 120,
            frame_end: 240,
            name: Some("立ち絵"),
            alias,
            effect_fingerprints: &[],
        })
    }

    fn sample_object_selector() -> ObjectSelector {
        ObjectSelector {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            scene_id: 0,
            layer: 2,
            frame: 120,
            name: Some("立ち絵".to_string()),
            fingerprint: sample_fingerprint("alias"),
            fingerprint_algorithm: FingerprintAlgorithm::GENERATED,
        }
    }

    fn sample_effect_selector() -> EffectSelector {
        EffectSelector {
            object: sample_object_selector(),
            effect_name: "動画ファイル".to_string(),
            effect_index: 0,
            fingerprint: sample_fingerprint("effect"),
            fingerprint_algorithm: FingerprintAlgorithm::GENERATED,
        }
    }

    #[test]
    fn object_selector_roundtrip() {
        let selector = sample_object_selector();
        let s = serde_json::to_string(&selector).unwrap();
        let restored: ObjectSelector = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, selector);
    }

    #[test]
    fn object_selector_allows_absent_name() {
        let selector = ObjectSelector {
            name: None,
            ..sample_object_selector()
        };
        let value = serde_json::to_value(&selector).unwrap();
        assert_eq!(value["name"], serde_json::Value::Null);
        let restored: ObjectSelector = serde_json::from_value(value).unwrap();
        assert_eq!(restored, selector);
    }

    #[test]
    fn object_selector_allows_unknown_optional_fields() {
        let mut value = serde_json::to_value(sample_object_selector()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::json!(1));
        let restored: ObjectSelector = serde_json::from_value(value).unwrap();
        assert_eq!(restored, sample_object_selector());
    }

    #[test]
    fn object_selector_rejects_malformed_fingerprint() {
        let mut value = serde_json::to_value(sample_object_selector()).unwrap();
        value.as_object_mut().unwrap().insert(
            "fingerprint".to_string(),
            serde_json::json!("md5:0123456789abcdef"),
        );
        let result: Result<ObjectSelector, _> = serde_json::from_value(value);
        assert!(result.is_err());
    }

    #[test]
    fn object_selector_keeps_invalid_epoch_as_string() {
        // 不正な epoch は書式ではなく前提条件の不整合として扱うため、
        // 逆直列化の時点では拒否しない。
        let mut value = serde_json::to_value(sample_object_selector()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("project_epoch".to_string(), serde_json::json!("not-a-uuid"));
        let restored: ObjectSelector = serde_json::from_value(value).unwrap();
        assert_eq!(restored.project_epoch, "not-a-uuid");
    }

    #[test]
    fn effect_selector_roundtrip() {
        let selector = sample_effect_selector();
        let s = serde_json::to_string(&selector).unwrap();
        let restored: EffectSelector = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, selector);
    }

    #[test]
    fn effect_selector_does_not_expose_name_index_form() {
        // 同名 effect の順序は名前と index の別フィールドで表し、
        // SDK 内部で用いる "name:index" 形式を利用者へ露出しない。
        let s = serde_json::to_string(&sample_effect_selector()).unwrap();
        assert!(!s.contains("動画ファイル:0"));
        let value = serde_json::to_value(sample_effect_selector()).unwrap();
        assert_eq!(value["effect_name"], serde_json::json!("動画ファイル"));
        assert_eq!(value["effect_index"], serde_json::json!(0));
    }

    #[test]
    fn effect_selector_allows_unknown_optional_fields() {
        let mut value = serde_json::to_value(sample_effect_selector()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::json!(1));
        let restored: EffectSelector = serde_json::from_value(value).unwrap();
        assert_eq!(restored, sample_effect_selector());
    }
}
