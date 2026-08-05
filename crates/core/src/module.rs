//! 登録済みモジュールの読み取り DTO と種別列挙。

use crate::kind::{kind_name, serialize_kind, visit_unknown_kind};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// 登録済みモジュール 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleEntry {
    /// モジュールの種別。
    pub module_type: ModuleType,
    /// モジュール名。
    pub name: String,
    /// ホストが利用者へ表示する説明文。
    pub information: String,
}

/// モジュールの種別。
///
/// 既知の種別は snake_case 文字列、未知の種別は
/// `{"type":"unknown","raw":<i32>}` として表現する。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ModuleType {
    /// フィルタスクリプト。
    ScriptFilter,
    /// オブジェクトスクリプト。
    ScriptObject,
    /// カメラスクリプト。
    ScriptCamera,
    /// トラックバースクリプト。
    ScriptTrack,
    /// スクリプトモジュール。
    ScriptModule,
    /// 入力プラグイン。
    PluginInput,
    /// 出力プラグイン。
    PluginOutput,
    /// フィルタプラグイン。
    PluginFilter,
    /// 汎用プラグイン。
    PluginGeneric,
    /// 未知の種別値を破棄せず raw 保持。
    ///
    /// **現時点でこの variant を作る経路は無い。** 種別値の解釈はより低い層で
    /// 行われ、解釈できない値を持つ項目はそこで落ちる。それでも型として持つのは、
    /// 低い層が raw を通すようになったときに、この型を変えずに受けられるように
    /// するためである。表せないことと、届かないことは別である。
    Unknown(i32),
}

impl ModuleType {
    /// 種別値を返す。
    pub fn as_raw(&self) -> i32 {
        match self {
            ModuleType::ScriptFilter => 1,
            ModuleType::ScriptObject => 2,
            ModuleType::ScriptCamera => 3,
            ModuleType::ScriptTrack => 4,
            ModuleType::ScriptModule => 5,
            ModuleType::PluginInput => 6,
            ModuleType::PluginOutput => 7,
            ModuleType::PluginFilter => 8,
            ModuleType::PluginGeneric => 9,
            ModuleType::Unknown(raw) => *raw,
        }
    }

    /// 種別値から復元する。既知でない値は [`ModuleType::Unknown`] とする。
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            1 => ModuleType::ScriptFilter,
            2 => ModuleType::ScriptObject,
            3 => ModuleType::ScriptCamera,
            4 => ModuleType::ScriptTrack,
            5 => ModuleType::ScriptModule,
            6 => ModuleType::PluginInput,
            7 => ModuleType::PluginOutput,
            8 => ModuleType::PluginFilter,
            9 => ModuleType::PluginGeneric,
            other => ModuleType::Unknown(other),
        }
    }

    fn name(&self) -> Option<&'static str> {
        match self {
            ModuleType::ScriptFilter => Some("script_filter"),
            ModuleType::ScriptObject => Some("script_object"),
            ModuleType::ScriptCamera => Some("script_camera"),
            ModuleType::ScriptTrack => Some("script_track"),
            ModuleType::ScriptModule => Some("script_module"),
            ModuleType::PluginInput => Some("plugin_input"),
            ModuleType::PluginOutput => Some("plugin_output"),
            ModuleType::PluginFilter => Some("plugin_filter"),
            ModuleType::PluginGeneric => Some("plugin_generic"),
            ModuleType::Unknown(_) => None,
        }
    }

    /// 種別を一意に表す名前を返す。
    ///
    /// 表現は [`fmt::Display`] と同じで、既知の種別は snake_case 名、未知の種別は
    /// raw 値を含む別形式になる。raw 値そのものではなく名前で識別するため、
    /// 既知の種別と同じ raw を持つ [`ModuleType::Unknown`] が既知の種別と
    /// 同じ表現になることはない。
    pub fn kind_name(&self) -> String {
        kind_name(self.name(), self.as_raw())
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "script_filter" => Some(ModuleType::ScriptFilter),
            "script_object" => Some(ModuleType::ScriptObject),
            "script_camera" => Some(ModuleType::ScriptCamera),
            "script_track" => Some(ModuleType::ScriptTrack),
            "script_module" => Some(ModuleType::ScriptModule),
            "plugin_input" => Some(ModuleType::PluginInput),
            "plugin_output" => Some(ModuleType::PluginOutput),
            "plugin_filter" => Some(ModuleType::PluginFilter),
            "plugin_generic" => Some(ModuleType::PluginGeneric),
            _ => None,
        }
    }
}

impl Serialize for ModuleType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_kind(self.name(), self.as_raw(), serializer)
    }
}

impl<'de> Deserialize<'de> for ModuleType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ModuleTypeVisitor;

        impl<'de> Visitor<'de> for ModuleTypeVisitor {
            type Value = ModuleType;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("モジュール種別の名前、または未知種別のオブジェクト")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                ModuleType::from_name(value)
                    .ok_or_else(|| E::custom(format!("未知のモジュール種別名です: {value}")))
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                visit_unknown_kind(map).map(ModuleType::from_raw)
            }
        }

        deserializer.deserialize_any(ModuleTypeVisitor)
    }
}

impl fmt::Display for ModuleType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.kind_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 既知の 9 種別。
    ///
    /// 期待値の表であり、[`ModuleType::from_raw`] からは導かない。導くと、写像を
    /// 書き換えたときに期待も一緒に動いて何も落ちない。
    const KNOWN: [(i32, &str, ModuleType); 9] = [
        (1, "script_filter", ModuleType::ScriptFilter),
        (2, "script_object", ModuleType::ScriptObject),
        (3, "script_camera", ModuleType::ScriptCamera),
        (4, "script_track", ModuleType::ScriptTrack),
        (5, "script_module", ModuleType::ScriptModule),
        (6, "plugin_input", ModuleType::PluginInput),
        (7, "plugin_output", ModuleType::PluginOutput),
        (8, "plugin_filter", ModuleType::PluginFilter),
        (9, "plugin_generic", ModuleType::PluginGeneric),
    ];

    #[test]
    fn known_module_types_map_between_raw_and_name() {
        for (raw, name, expected) in KNOWN {
            assert_eq!(ModuleType::from_raw(raw), expected, "raw {raw}");
            assert_eq!(expected.as_raw(), raw, "{name}");
            assert_eq!(
                serde_json::to_value(&expected).unwrap(),
                serde_json::json!(name)
            );
            assert_eq!(
                serde_json::from_value::<ModuleType>(serde_json::json!(name)).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn an_unknown_module_type_keeps_its_raw_value() {
        // 到達経路は無いが、型としては未知値を運べる。運べない型にすると、
        // 値を通せるようになったときに型ごと作り直すことになる。
        let unknown = ModuleType::Unknown(99);
        assert_eq!(unknown.as_raw(), 99);
        assert_eq!(ModuleType::from_raw(99), unknown);
        let json = serde_json::to_value(&unknown).unwrap();
        assert_eq!(json, serde_json::json!({ "type": "unknown", "raw": 99 }));
        assert_eq!(serde_json::from_value::<ModuleType>(json).unwrap(), unknown);
    }

    #[test]
    fn an_unknown_module_type_is_told_apart_from_the_known_type_of_the_same_raw() {
        for (raw, _, known) in KNOWN {
            let unknown = ModuleType::Unknown(raw);
            assert_ne!(unknown, known, "raw {raw}");
            assert_ne!(unknown.kind_name(), known.kind_name(), "raw {raw}");
        }
    }

    #[test]
    fn an_unknown_module_type_name_is_rejected() {
        assert!(serde_json::from_value::<ModuleType>(serde_json::json!("script_unknown")).is_err());
    }

    #[test]
    fn module_entry_roundtrip() {
        let entry = ModuleEntry {
            module_type: ModuleType::PluginInput,
            name: "input.aui2".to_string(),
            information: "入力プラグイン version 1.0".to_string(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["module_type"], serde_json::json!("plugin_input"));
        assert_eq!(serde_json::from_value::<ModuleEntry>(json).unwrap(), entry);
    }
}
