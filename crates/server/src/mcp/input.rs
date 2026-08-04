//! read tool の入力型と、read / 編集が共有するセレクター。
//!
//! 未知フィールドを拒否し、文字列長・整数範囲を schema で制約する。
//! ページ指定は IPC の params と同じ平坦な形（`offset` / `limit` /
//! `snapshot_revision`）で受け取る。
//!
//! 例外は [`ObjectSelectorInput`] と [`EffectSelectorInput`] で、応答が返した値を
//! そのまま送り返す双方向の値であるため未知フィールドを拒否しない。**この 2 つは
//! 編集 tool の入力型（[`crate::mcp::edit_input`]）も用いる。** 同じ値を読み取りの
//! 応答から受け取って編集へ送り返す往復型であり、族ごとに別の型を持てば同じ値の
//! 検証が 2 通りになる。
//!
//! schema の制約は宣言であり、要求がそれを満たすかどうかは検証されない。
//! 宣言した制約は本モジュールで実際に検証し、違反を `invalid_argument` として
//! 接続前に返す。検証を省くと、過大な入力が接続先へ送られてフレーム長の上限で
//! 落ち、要求の誤りが再試行可能な転送の失敗として報告されてしまう。
//!
//! 対応は次のとおり。
//! - `limit` の範囲: [`build_page_request`]
//! - `instance_id` の長さと書式: [`parse_instance_id`]
//! - `selector` の各文字列長: [`ObjectSelectorInput::validate`]
//! - `selector.fingerprint` の書式: [`ObjectSelectorInput::to_selector`]
//! - `effect` の各文字列長と fingerprint の書式: [`EffectSelectorInput::to_selector`]
//! - `frames` / `items` の件数と項目名: [`GetEffectItemValuesInput::to_params`]

use crate::mcp::failure::{from_code, invalid_argument};
use aviutl2_mcp_core::{
    DEFAULT_PAGE_LIMIT, EffectSelector, EffectType, ErrorCode, ErrorObject, FiniteF64,
    GetEffectItemValuesParams, GetObjectParams, InstanceId, ListAvailableEffectsParams,
    ListLayersParams, ListObjectsParams, MAX_EVALUATED_FRAMES, MAX_EVALUATED_ITEMS, MAX_PAGE_LIMIT,
    ObjectFilter, ObjectSelector, PageRequest,
};
use schemars::JsonSchema;
use serde::Deserialize;

/// `instance_id` が満たすべき UUID の書式。
pub(crate) const UUID_PATTERN: &str =
    r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$";

/// [`UUID_PATTERN`] が定める各群の文字数。
const UUID_GROUP_LENGTHS: [usize; 5] = [8, 4, 4, 4, 12];

/// fingerprint が満たすべき書式。
///
/// 前置と桁数はダイジェストの表現に共通のものだが、`#[schemars(pattern(..))]`
/// が定数式しか取らないため、ここでは書き下している。共有の定義との一致は
/// 試験で突き合わせる。
pub(crate) const FINGERPRINT_PATTERN: &str = r"^sha256:[0-9a-f]{64}$";

/// オブジェクト名・レイヤー名に許す最大文字数。
pub(crate) const MAX_NAME_CHARS: u32 = 1_024;

/// プロジェクト epoch に許す最大文字数。
pub(crate) const MAX_EPOCH_CHARS: u32 = 64;

fn default_limit() -> u32 {
    DEFAULT_PAGE_LIMIT
}

/// `list_instances` の入力。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListInstancesInput {
    /// 取得を開始する 0 始まりの位置。
    #[serde(default)]
    pub offset: u32,
    /// 取得件数。
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: u32,
}

/// インスタンスを 1 つ指定するだけの入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstanceInput {
    /// 対象インスタンスの ID。list_instances が返す値を指定する。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
}

/// ページ指定。IPC の params と同じ平坦な形で受け取る。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageInput {
    /// 取得を開始する 0 始まりの位置。
    #[serde(default)]
    pub offset: u32,
    /// 取得件数。
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: u32,
    /// 先頭ページが返した snapshot_revision。指定すると一致しない場合に precondition_failed となる。
    #[serde(default)]
    pub snapshot_revision: Option<u64>,
}

impl PageInput {
    /// 共通のページ要求へ変換する。
    fn to_page_request(self) -> Result<PageRequest, ErrorObject> {
        build_page_request(self.offset, self.limit, self.snapshot_revision)
    }
}

/// effect カタログ列挙のページ指定。
///
/// 形は [`PageInput`] と同じだが、`snapshot_revision` の意味づけだけが異なる。
/// effect カタログは登録済みプラグインの集合であり、プロジェクトの revision に
/// 連動しない。照合すると、カタログと無関係な編集で revision が進んだだけで
/// 2 ページ目以降が失敗する誤検知になる一方、カタログ自身の変化は revision に
/// 現れないため取りこぼしも防げない。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AvailableEffectsPageInput {
    /// 取得を開始する 0 始まりの位置。
    #[serde(default)]
    pub offset: u32,
    /// 取得件数。
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: u32,
    /// 先頭ページが返した snapshot_revision。この tool では照合に用いない。effect カタログは登録済みプラグインの集合でありプロジェクトの revision に連動しないためである。
    #[serde(default)]
    pub snapshot_revision: Option<u64>,
}

impl AvailableEffectsPageInput {
    /// 共通のページ要求へ変換する。
    fn to_page_request(self) -> Result<PageRequest, ErrorObject> {
        build_page_request(self.offset, self.limit, self.snapshot_revision)
    }
}

/// ページ指定を検証してページ要求へ変換する。
fn build_page_request(
    offset: u32,
    limit: u32,
    snapshot_revision: Option<u64>,
) -> Result<PageRequest, ErrorObject> {
    let request = PageRequest {
        offset,
        limit,
        snapshot_revision,
    };
    request.validate().map_err(|_| {
        invalid_argument(format!(
            "limit は 1 以上 {MAX_PAGE_LIMIT} 以下である必要があります"
        ))
    })?;
    Ok(request)
}

/// `list_layers` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListLayersInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 列挙対象が現在シーンのままであることを確認するためのシーン ID。
    pub expected_scene_id: i32,
    /// ページ指定。
    #[serde(flatten)]
    pub page: PageInput,
}

/// `list_objects` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListObjectsInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 列挙対象が現在シーンのままであることを確認するためのシーン ID。
    pub expected_scene_id: i32,
    /// レイヤー範囲による絞り込み。
    #[serde(default)]
    pub filter: Option<ObjectFilterInput>,
    /// ページ指定。
    #[serde(flatten)]
    pub page: PageInput,
}

/// オブジェクト列挙の絞り込み条件。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectFilterInput {
    /// 対象とする最小のレイヤー番号。0 始まり。
    #[serde(default)]
    pub layer_min: Option<u32>,
    /// 対象とする最大のレイヤー番号。0 始まり。
    #[serde(default)]
    pub layer_max: Option<u32>,
}

/// `get_object` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetObjectInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象オブジェクトのセレクター。list_objects が返した値をそのまま指定する。
    pub selector: ObjectSelectorInput,
}

/// オブジェクトを再指定するセレクター。
///
/// 応答が返した値をそのまま送り返す双方向の値であり、未知フィールドを拒否しない。
/// server はこの値を解釈せず接続先へ転送するだけなので、ここで弾いても得るものが
/// 無い一方、フィールドが増えた応答をそのまま渡すクライアントを入口で
/// `invalid_argument` にしてしまう。既知フィールドの検証は従来どおり行う。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ObjectSelectorInput {
    /// 応答が返したプロジェクトの epoch。
    #[schemars(length(min = 1, max = MAX_EPOCH_CHARS))]
    pub project_epoch: String,
    /// 読み取り時と同じシーンかを確認するためのシーン ID。
    pub scene_id: i32,
    /// 0 始まりのレイヤー番号。
    pub layer: u32,
    /// 0 始まりの開始フレーム番号。
    pub frame: u32,
    /// オブジェクト名。標準名のままなら null。
    #[serde(default)]
    #[schemars(length(max = MAX_NAME_CHARS))]
    pub name: Option<String>,
    /// 同一性検証用の fingerprint。
    #[schemars(pattern(FINGERPRINT_PATTERN))]
    pub fingerprint: String,
}

/// `list_available_effects` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListAvailableEffectsInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 種別による絞り込み。
    #[serde(default)]
    pub effect_type: Option<EffectTypeInput>,
    /// ページ指定。
    #[serde(flatten)]
    pub page: AvailableEffectsPageInput,
}

/// オブジェクト内の effect を再指定するセレクター。
///
/// [`ObjectSelectorInput`] と同じく往復型であり、未知フィールドを拒否しない。
/// 内側の `object` も同じ扱いになる。fingerprint の算出方式は `object` だけが
/// 持ち、ここには置かない。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct EffectSelectorInput {
    /// effect が属するオブジェクト。
    pub object: ObjectSelectorInput,
    /// effect 名。
    #[schemars(length(max = MAX_NAME_CHARS))]
    pub effect_name: String,
    /// 同名 effect のうち何番目か。0 始まり。
    pub effect_index: u32,
    /// 同一性検証用の fingerprint。
    #[schemars(pattern(FINGERPRINT_PATTERN))]
    pub fingerprint: String,
}

impl EffectSelectorInput {
    /// セレクターへ変換する。文字数と fingerprint の書式はここで検証される。
    pub(crate) fn to_selector(&self) -> Result<EffectSelector, ErrorObject> {
        let object = self.object.to_selector()?;
        ensure_length("selector.effect_name", &self.effect_name, 0, MAX_NAME_CHARS)?;
        let object = serde_json::to_value(&object).map_err(|_| {
            from_code(
                ErrorCode::InternalError,
                "selector を組み立てられませんでした",
            )
        })?;
        let value = serde_json::json!({
            "object": object,
            "effect_name": self.effect_name,
            "effect_index": self.effect_index,
            "fingerprint": self.fingerprint,
        });
        serde_json::from_value(value).map_err(|_| {
            invalid_argument(
                "selector を解釈できません。get_object が返した effect の selector をそのまま指定してください",
            )
        })
    }
}

/// `get_effect_item_values` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetEffectItemValuesInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 評価対象の effect のセレクター。get_object が返した effect の selector をそのまま指定する。
    pub effect: EffectSelectorInput,
    /// 評価するフレーム番号。シーンの絶対フレーム番号で 0 始まり。小数を指定するとフレーム間の位置を指す。
    #[schemars(length(min = 1, max = MAX_FRAME_COUNT))]
    pub frames: Vec<f64>,
    /// 評価する設定項目名。省略すると effect のトラックバー項目とチェックボックス項目すべてが対象になる。
    #[serde(default)]
    #[schemars(length(min = 1, max = MAX_ITEM_COUNT))]
    pub items: Option<Vec<String>>,
}

/// 1 度に評価できるフレームの最大件数。
const MAX_FRAME_COUNT: u32 = MAX_EVALUATED_FRAMES as u32;

/// 1 度に評価できる設定項目の最大件数。
const MAX_ITEM_COUNT: u32 = MAX_EVALUATED_ITEMS as u32;

impl GetEffectItemValuesInput {
    /// IPC の params へ変換する。
    ///
    /// 件数と項目名の検証は core の実装を呼ぶ。要求元と実行側が同じ判定を
    /// 共有し、宣言した制約を接続前に実際へ確かめる。
    pub fn to_params(&self) -> Result<GetEffectItemValuesParams, ErrorObject> {
        let mut frames = Vec::with_capacity(self.frames.len());
        for frame in &self.frames {
            frames.push(FiniteF64::try_new(*frame).ok_or_else(|| {
                invalid_argument("frames には有限の数値を指定する必要があります")
            })?);
        }
        let params = GetEffectItemValuesParams {
            effect: self.effect.to_selector()?,
            frames,
            items: self.items.clone(),
        };
        params
            .validate()
            .map_err(|error| invalid_argument(error.to_string()))?;
        Ok(params)
    }
}

/// 絞り込みに指定できる effect の種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectTypeInput {
    Filter,
    Input,
    Transition,
    Control,
    Output,
}

impl From<EffectTypeInput> for EffectType {
    fn from(value: EffectTypeInput) -> Self {
        match value {
            EffectTypeInput::Filter => EffectType::Filter,
            EffectTypeInput::Input => EffectType::Input,
            EffectTypeInput::Transition => EffectType::Transition,
            EffectTypeInput::Control => EffectType::Control,
            EffectTypeInput::Output => EffectType::Output,
        }
    }
}

/// 文字列が宣言した文字数の範囲に収まることを確かめる。
///
/// 長さは JSON Schema の `minLength` / `maxLength` と同じく文字数で数える。
/// 違反した値そのものは説明へ含めない。過大な入力をそのまま応答へ写すと、
/// 入力の誤りを伝える応答自体が過大になる。
pub(crate) fn ensure_length(
    field: &str,
    value: &str,
    min: u32,
    max: u32,
) -> Result<(), ErrorObject> {
    let length = value.chars().count();
    if length < min as usize || length > max as usize {
        return Err(invalid_argument(format!(
            "{field} は {min} 文字以上 {max} 文字以下である必要があります"
        )));
    }
    Ok(())
}

/// [`UUID_PATTERN`] に一致するかを判定する。
///
/// 群の長さが定まるため、一致すれば全体の長さも 36 文字に定まる。中括弧付きや
/// URN 形式のような他の UUID 表記はここで弾かれる。
fn is_canonical_uuid(value: &str) -> bool {
    let mut groups = value.split('-');
    for length in UUID_GROUP_LENGTHS {
        let Some(group) = groups.next() else {
            return false;
        };
        if group.len() != length || !group.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
    }
    groups.next().is_none()
}

/// `instance_id` 文字列を識別子へ変換する。
///
/// 書式を先に確かめることで、schema が宣言する長さと書式の双方を検証する。
pub fn parse_instance_id(value: &str) -> Result<InstanceId, ErrorObject> {
    if !is_canonical_uuid(value) {
        return Err(invalid_argument(
            "instance_id はハイフン区切りの UUID である必要があります",
        ));
    }
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .map_err(|_| invalid_argument("instance_id は UUID である必要があります"))
}

impl ListInstancesInput {
    /// ページ要求へ変換する。
    pub fn to_page_request(self) -> Result<PageRequest, ErrorObject> {
        PageInput {
            offset: self.offset,
            limit: self.limit,
            snapshot_revision: None,
        }
        .to_page_request()
    }
}

impl ListLayersInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<ListLayersParams, ErrorObject> {
        Ok(ListLayersParams {
            expected_scene_id: self.expected_scene_id,
            page: self.page.to_page_request()?,
        })
    }
}

impl ListObjectsInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<ListObjectsParams, ErrorObject> {
        let filter = self.filter.map(ObjectFilterInput::to_filter).transpose()?;
        Ok(ListObjectsParams {
            expected_scene_id: self.expected_scene_id,
            filter,
            page: self.page.to_page_request()?,
        })
    }
}

impl ObjectFilterInput {
    /// 絞り込み条件へ変換し、範囲の整合を検証する。
    fn to_filter(self) -> Result<ObjectFilter, ErrorObject> {
        let filter = ObjectFilter {
            layer_min: self.layer_min.map(|v| v as usize),
            layer_max: self.layer_max.map(|v| v as usize),
        };
        filter
            .validate()
            .map_err(|e| invalid_argument(e.to_string()))?;
        Ok(filter)
    }
}

impl GetObjectInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<GetObjectParams, ErrorObject> {
        Ok(GetObjectParams {
            selector: self.selector.to_selector()?,
        })
    }
}

impl ObjectSelectorInput {
    /// 各フィールドが schema で宣言した文字数の範囲に収まることを確かめる。
    fn validate(&self) -> Result<(), ErrorObject> {
        ensure_length(
            "selector.project_epoch",
            &self.project_epoch,
            1,
            MAX_EPOCH_CHARS,
        )?;
        if let Some(name) = &self.name {
            ensure_length("selector.name", name, 0, MAX_NAME_CHARS)?;
        }
        Ok(())
    }

    /// セレクターへ変換する。文字数と fingerprint の書式はここで検証される。
    pub(crate) fn to_selector(&self) -> Result<ObjectSelector, ErrorObject> {
        self.validate()?;
        let value = serde_json::json!({
            "project_epoch": self.project_epoch,
            "scene_id": self.scene_id,
            "layer": self.layer,
            "frame": self.frame,
            "name": self.name,
            "fingerprint": self.fingerprint,
        });
        serde_json::from_value(value).map_err(|_| {
            invalid_argument(
                "selector を解釈できません。list_objects が返した値をそのまま指定してください",
            )
        })
    }
}

impl ListAvailableEffectsInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<ListAvailableEffectsParams, ErrorObject> {
        Ok(ListAvailableEffectsParams {
            effect_type: self.effect_type.map(EffectType::from),
            page: self.page.to_page_request()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::{ErrorCode, SHA256_HEX_LEN, SHA256_PREFIX};

    const SAMPLE_ID: &str = "8df98c04-e7c2-4f98-b3ce-fc1c39d76414";
    const SAMPLE_FINGERPRINT: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn selector_json() -> serde_json::Value {
        serde_json::json!({
            "project_epoch": "78be92d1-c8c9-44c6-ae52-387548971468",
            "scene_id": 0,
            "layer": 2,
            "frame": 120,
            "name": "立ち絵",
            "fingerprint": SAMPLE_FINGERPRINT,
        })
    }

    #[test]
    fn the_fingerprint_pattern_agrees_with_the_shared_digest_form() {
        // 書式を書き下している唯一の場所である。共有の定義を変えた人がここへ
        // 辿り着けるよう、両者を突き合わせる。
        assert!(
            FINGERPRINT_PATTERN.starts_with(&format!("^{SHA256_PREFIX}")),
            "{FINGERPRINT_PATTERN}"
        );
        assert!(
            FINGERPRINT_PATTERN.contains(&format!("{{{SHA256_HEX_LEN}}}")),
            "{FINGERPRINT_PATTERN}"
        );
    }

    #[test]
    fn list_instances_input_defaults_match_page_defaults() {
        let input: ListInstancesInput = serde_json::from_str("{}").expect("省略を受理する");
        assert_eq!(input.offset, 0);
        assert_eq!(input.limit, DEFAULT_PAGE_LIMIT);
    }

    #[test]
    fn list_instances_input_rejects_unknown_field() {
        assert!(serde_json::from_str::<ListInstancesInput>(r#"{"snapshot_revision":1}"#).is_err());
    }

    #[test]
    fn list_instances_input_rejects_limit_out_of_range() {
        for limit in [0, MAX_PAGE_LIMIT + 1] {
            let input = ListInstancesInput { offset: 0, limit };
            let error = input.to_page_request().expect_err("範囲外は拒否される");
            assert_eq!(error.code, ErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn page_fields_are_accepted_flat() {
        let input: ListLayersInput = serde_json::from_value(serde_json::json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "offset": 5,
            "limit": 10,
            "snapshot_revision": 7,
        }))
        .expect("平坦なページ指定を受理する");
        let params = input.to_params().expect("params へ変換できる");
        assert_eq!(params.expected_scene_id, 3);
        assert_eq!(params.page.offset, 5);
        assert_eq!(params.page.limit, 10);
        assert_eq!(params.page.snapshot_revision, Some(7));
    }

    #[test]
    fn list_layers_input_rejects_unknown_field() {
        assert!(
            serde_json::from_value::<ListLayersInput>(serde_json::json!({
                "instance_id": SAMPLE_ID,
                "expected_scene_id": 0,
                "future": 1,
            }))
            .is_err()
        );
    }

    #[test]
    fn list_layers_input_requires_instance_id() {
        assert!(
            serde_json::from_value::<ListLayersInput>(
                serde_json::json!({ "expected_scene_id": 0 })
            )
            .is_err()
        );
    }

    #[test]
    fn instance_input_requires_instance_id() {
        assert!(serde_json::from_str::<InstanceInput>("{}").is_err());
    }

    #[test]
    fn instance_id_must_be_uuid() {
        assert!(parse_instance_id("not-a-uuid").is_err());
        assert!(parse_instance_id(SAMPLE_ID).is_ok());
    }

    #[test]
    fn instance_id_must_match_the_declared_form() {
        // schema は長さ 36 とハイフン区切りの十六進を宣言している。他の UUID
        // 表記や過大な入力を受け付けると、宣言と実際の受理範囲が食い違う。
        for value in [
            "",
            &format!("{{{SAMPLE_ID}}}"),
            &format!("urn:uuid:{SAMPLE_ID}"),
            &SAMPLE_ID.replace('-', ""),
            &format!("{SAMPLE_ID} "),
            &"9".repeat(100_000),
        ] {
            let error = parse_instance_id(value).expect_err("宣言外の書式は拒否される");
            assert_eq!(error.code, ErrorCode::InvalidArgument);
        }
        assert_eq!(SAMPLE_ID.chars().count(), 36);
        assert!(is_canonical_uuid(SAMPLE_ID));
    }

    #[test]
    fn selector_strings_are_bounded_before_the_request_is_sent() {
        // schema が宣言する上限を超える値は接続前に拒否する。接続先へ送ると
        // フレーム長の上限で落ち、要求の誤りが転送の失敗として報告される。
        let cases = [
            ("project_epoch", "x".repeat(MAX_EPOCH_CHARS as usize + 1)),
            ("name", "あ".repeat(MAX_NAME_CHARS as usize + 1)),
        ];

        for (field, value) in cases {
            let mut selector = selector_json();
            selector[field] = serde_json::json!(value);
            let input = GetObjectInput {
                instance_id: SAMPLE_ID.to_string(),
                selector: serde_json::from_value(selector).expect("入力型としては受理される"),
            };
            let error = input
                .to_params()
                .err()
                .unwrap_or_else(|| panic!("{field} の上限超過が受理されました"));
            assert_eq!(error.code, ErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn selector_strings_reject_empty_where_declared() {
        let mut selector = selector_json();
        selector["project_epoch"] = serde_json::json!("");
        let input = GetObjectInput {
            instance_id: SAMPLE_ID.to_string(),
            selector: serde_json::from_value(selector).expect("入力型としては受理される"),
        };
        let error = input
            .to_params()
            .expect_err("project_epoch の空文字列が受理されました");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn selector_within_the_declared_bounds_is_accepted() {
        let mut selector = selector_json();
        selector["name"] = serde_json::json!("あ".repeat(MAX_NAME_CHARS as usize));
        selector["project_epoch"] = serde_json::json!("e".repeat(MAX_EPOCH_CHARS as usize));
        let input = GetObjectInput {
            instance_id: SAMPLE_ID.to_string(),
            selector: serde_json::from_value(selector).expect("入力型としては受理される"),
        };
        assert!(input.to_params().is_ok(), "上限ちょうどが拒否されました");
    }

    #[test]
    fn length_is_counted_in_characters() {
        assert_eq!(ensure_length("f", "あああ", 1, 3), Ok(()));
        assert!(ensure_length("f", "あああ", 1, 2).is_err());
        assert!(ensure_length("f", "", 1, 2).is_err());
        assert_eq!(ensure_length("f", "", 0, 2), Ok(()));
    }

    #[test]
    fn object_filter_rejects_inverted_range() {
        let input = ListObjectsInput {
            instance_id: SAMPLE_ID.to_string(),
            expected_scene_id: 0,
            filter: Some(ObjectFilterInput {
                layer_min: Some(8),
                layer_max: Some(1),
            }),
            page: PageInput {
                offset: 0,
                limit: DEFAULT_PAGE_LIMIT,
                snapshot_revision: None,
            },
        };
        let error = input.to_params().expect_err("逆転した範囲は拒否される");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn object_filter_rejects_unknown_field() {
        assert!(
            serde_json::from_value::<ObjectFilterInput>(serde_json::json!({ "layer": 1 })).is_err()
        );
    }

    #[test]
    fn get_object_input_converts_selector() {
        let input: GetObjectInput = serde_json::from_value(serde_json::json!({
            "instance_id": SAMPLE_ID,
            "selector": selector_json(),
        }))
        .expect("セレクターを受理する");
        let params = input.to_params().expect("params へ変換できる");
        assert_eq!(params.selector.layer, 2);
        assert_eq!(params.selector.frame, 120);
        assert_eq!(params.selector.fingerprint.as_str(), SAMPLE_FINGERPRINT);
    }

    #[test]
    fn get_object_input_rejects_malformed_fingerprint() {
        let mut selector = selector_json();
        selector["fingerprint"] = serde_json::json!("sha256:zzzz");
        let input = GetObjectInput {
            instance_id: SAMPLE_ID.to_string(),
            selector: serde_json::from_value(selector).expect("入力型としては受理される"),
        };
        let error = input
            .to_params()
            .expect_err("書式違反の fingerprint は拒否される");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn get_object_input_accepts_unknown_selector_field() {
        // 応答へフィールドが増えても、返された selector をそのまま渡すクライアントを
        // 入口で拒否しない。既知フィールドは失われずに接続先へ届く。
        let mut selector = selector_json();
        selector["future"] = serde_json::json!(1);
        let input: GetObjectInput = serde_json::from_value(serde_json::json!({
            "instance_id": SAMPLE_ID,
            "selector": selector,
        }))
        .expect("未知フィールドを含む selector を受理する");

        let params = input.to_params().expect("params へ変換できる");
        assert_eq!(
            params.selector.project_epoch,
            "78be92d1-c8c9-44c6-ae52-387548971468"
        );
        assert_eq!(params.selector.scene_id, 0);
        assert_eq!(params.selector.layer, 2);
        assert_eq!(params.selector.frame, 120);
        assert_eq!(params.selector.name.as_deref(), Some("立ち絵"));
        assert_eq!(params.selector.fingerprint.as_str(), SAMPLE_FINGERPRINT);
    }

    #[test]
    fn get_object_input_ignores_a_fingerprint_algorithm() {
        // 算出方式は要求の一部ではない。往復型は未知フィールドを拒否しないため
        // 名乗る指定も受理されるが、値は接続先へ渡らずに捨てられる。
        let mut selector = selector_json();
        selector["fingerprint_algorithm"] = serde_json::json!("sha256-alias-v1");
        let input: GetObjectInput = serde_json::from_value(serde_json::json!({
            "instance_id": SAMPLE_ID,
            "selector": selector,
        }))
        .expect("算出方式を名乗る selector を受理する");

        let params = input.to_params().expect("params へ変換できる");
        let sent = serde_json::to_value(&params.selector).expect("直列化できる");
        assert!(
            sent.get("fingerprint_algorithm").is_none(),
            "{sent} が算出方式を運んでいます"
        );
    }

    #[test]
    fn get_object_input_rejects_unknown_top_level_field() {
        assert!(
            serde_json::from_value::<GetObjectInput>(serde_json::json!({
                "instance_id": SAMPLE_ID,
                "selector": selector_json(),
                "future": 1,
            }))
            .is_err()
        );
    }

    #[test]
    fn effect_type_input_maps_to_core_type() {
        let input: ListAvailableEffectsInput = serde_json::from_value(serde_json::json!({
            "instance_id": SAMPLE_ID,
            "effect_type": "filter",
        }))
        .expect("種別名を受理する");
        let params = input.to_params().expect("params へ変換できる");
        assert_eq!(params.effect_type, Some(EffectType::Filter));
    }

    #[test]
    fn available_effects_page_still_accepts_snapshot_revision() {
        // 応答が返した値をそのまま送り返すクライアントを弾かない。
        let input: ListAvailableEffectsInput = serde_json::from_value(serde_json::json!({
            "instance_id": SAMPLE_ID,
            "offset": 10,
            "limit": 20,
            "snapshot_revision": 5,
        }))
        .expect("snapshot_revision を受理する");
        let params = input.to_params().expect("params へ変換できる");
        assert_eq!(params.page.offset, 10);
        assert_eq!(params.page.limit, 20);
        assert_eq!(params.page.snapshot_revision, Some(5));
    }

    #[test]
    fn available_effects_page_rejects_unknown_field() {
        assert!(
            serde_json::from_value::<ListAvailableEffectsInput>(serde_json::json!({
                "instance_id": SAMPLE_ID,
                "future": 1,
            }))
            .is_err()
        );
    }

    #[test]
    fn effect_type_input_rejects_unknown_value() {
        assert!(
            serde_json::from_value::<ListAvailableEffectsInput>(serde_json::json!({
                "instance_id": SAMPLE_ID,
                "effect_type": "unknown",
            }))
            .is_err()
        );
    }
}
