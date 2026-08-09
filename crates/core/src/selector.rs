//! opaque handle を公開せずに対象を再指定するセレクター。

use crate::fingerprint::Fingerprint;
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
    /// オブジェクト名。標準名のままなら null。
    ///
    /// 候補の絞り込みには使わない。対象はレイヤーと開始フレームで定まり、
    /// 同一性は fingerprint が検証する。名前は fingerprint の材料であるため、
    /// 名前が変わった対象は fingerprint の照合が捕まえる。
    pub name: Option<String>,
    /// 同一性検証用の fingerprint。
    ///
    /// 算出方式は運ばない。方式は digest の材料であり、方式が違えば digest も
    /// 違うため、方式の食い違いは fingerprint の照合が捕まえる。
    pub fingerprint: Fingerprint,
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
        }
    }

    fn sample_effect_selector() -> EffectSelector {
        EffectSelector {
            object: sample_object_selector(),
            effect_name: "動画ファイル".to_string(),
            effect_index: 0,
            fingerprint: sample_fingerprint("effect"),
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
    fn object_selector_ignores_a_fingerprint_algorithm() {
        // 往復型は未知フィールドを拒否しない。方式を名乗る指定も拒否されず、
        // 値は解釈されずに捨てられる。
        let mut value = serde_json::to_value(sample_object_selector()).unwrap();
        value.as_object_mut().unwrap().insert(
            "fingerprint_algorithm".to_string(),
            serde_json::json!("sha256-alias-v1"),
        );
        let restored: ObjectSelector = serde_json::from_value(value).unwrap();
        assert_eq!(restored, sample_object_selector());
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
    fn selectors_do_not_carry_a_fingerprint_algorithm() {
        // 方式は digest の材料であって運ぶ値ではない。読まれない値を要求元へ
        // 組み立てさせない。
        for value in [
            serde_json::to_value(sample_object_selector()).unwrap(),
            serde_json::to_value(sample_effect_selector()).unwrap(),
        ] {
            assert!(
                value.get("fingerprint_algorithm").is_none(),
                "{value} が算出方式を持っています"
            );
        }
    }

    #[test]
    fn effect_selector_does_not_carry_the_column_position() {
        // 列の位置は effect の増減で変わる。往復用トークンへ入れると、位置が
        // ずれただけで再指定が前提条件の不一致になる。
        let value = serde_json::to_value(sample_effect_selector()).unwrap();
        assert!(
            value.get("position").is_none(),
            "{value} が列の位置を持っています"
        );
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
