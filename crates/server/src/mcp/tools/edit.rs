//! 編集 tool。

use crate::mcp::describe;
use crate::mcp::edit_input::{
    AddEffectInput, ApplyBatchInput, CreateObjectInput, CreateObjectSectionInput,
    DeleteEffectInput, DeleteObjectInput, DeleteObjectSectionInput, MoveEffectInput,
    MoveObjectInput, MoveObjectSectionInput, SetEffectEnabledInput, SetGridBpmInput,
    SetLayerStateInput, SetObjectItemInput, SetObjectNameInput, SetSceneSettingsInput,
    SetSelectionInput,
};
use crate::mcp::server::AviUtl2McpServer;
use aviutl2_mcp_core::{
    OPERATION_ADD_EFFECT, OPERATION_APPLY_BATCH, OPERATION_CREATE_OBJECT,
    OPERATION_CREATE_OBJECT_SECTION, OPERATION_DELETE_EFFECT, OPERATION_DELETE_OBJECT,
    OPERATION_DELETE_OBJECT_SECTION, OPERATION_MOVE_EFFECT, OPERATION_MOVE_OBJECT,
    OPERATION_MOVE_OBJECT_SECTION, OPERATION_SET_EFFECT_ENABLED, OPERATION_SET_GRID_BPM,
    OPERATION_SET_LAYER_STATE, OPERATION_SET_OBJECT_ITEM, OPERATION_SET_OBJECT_NAME,
    OPERATION_SET_SCENE_SETTINGS, OPERATION_SET_SELECTION,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

#[tool_router(router = edit_tools_router, vis = "pub(in crate::mcp)")]
impl AviUtl2McpServer {
    /// メディアファイル・object alias・エフェクト名・登録済みエイリアス名のいずれかから
    /// オブジェクトを作成する。
    /// source が effect のとき、カタログに在る名前でも作成元にできるとは限らず、
    /// その場合は unsupported_operation（effect_not_creatable）となる。
    /// 名前がカタログに無い場合は unsupported_operation（effect_not_registered）となる。
    /// 表として読めないエイリアスは、source が alias_name でも object_alias でも
    /// invalid_argument（alias_not_parsable）で拒否される。
    /// source が alias_name のとき、effect を 1 つも含まないエイリアスは
    /// invalid_argument（alias_without_effect）で拒否される。
    /// source が object_alias のとき、移動行は設定項目へ書くときと同じ検証を通り、通らない行は
    /// invalid_argument（track_flags_not_representable / track_mode_unknown / track_mode_not_writable / track_value_count）で拒否される。
    /// source が object_alias のとき、テキスト種別（text / string）の設定項目の行は `\` の綴りを検査され、
    /// `\` の次が `n` でも `\` でもない行は invalid_argument（unescaped_backslash）で拒否される。
    /// 行の拒否は details.item に項目名を載せ、節に属する行では details.heading に節の見出しを載せる。
    /// これらの拒否はいずれも作成より前に起き、オブジェクトは 1 つも作られない。
    /// 複数オブジェクトを含む alias は全てが作成され、created に全件、object に
    /// その先頭が入る。応答の effect は常に null である。
    /// 長さと挿入位置はホストが自動調整し得るため、
    /// 応答が返す位置は要求した宛先と異なり得る。
    /// 応答が返す selector が実際の配置であり、配置を確かめるには応答の値を見る。
    /// 複数オブジェクトでは created の全件が対象である。宛先が 1 件でも埋まって
    /// いると alias 全体が同じだけずれ、相対の構造だけが保たれる。これは失敗に
    /// ならず応答は成功であるため、placement_adjusted が真なら created を全件見る。
    /// 同じ要求を再送すると重複して作成し得る。作成先に既存オブジェクトがあれば
    /// precondition_failed（destination_occupied）となるため通常は防がれるが、
    /// ホストが挿入位置を自動調整した場合はすり抜け得る。
    /// 配置先のレイヤーがロックされている場合は precondition_failed（layer_locked）と
    /// なる。set_layer_state でロックを解除してから再実行する。
    #[tool(
        name = "create_object",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::create_object()
        )
    )]
    pub async fn create_object(
        &self,
        Parameters(input): Parameters<CreateObjectInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "create_object",
            OPERATION_CREATE_OBJECT,
            instance_id,
            move || input.to_params(),
            describe::create_object,
        )
        .await
    }

    /// オブジェクトのレイヤーと開始フレームを変更する。
    /// 配置はホストが調整し得るため、応答が返す位置は要求した宛先と異なり得る。
    /// 応答が返す selector が実際の配置であり、配置を確かめるには応答の値を見る。
    /// 宛先に既存オブジェクトがある場合は precondition_failed（destination_occupied）となる。
    /// 移動元または移動先のレイヤーがロックされている場合は
    /// precondition_failed（layer_locked）となる。set_layer_state で
    /// ロックを解除してから再実行する。
    #[tool(
        name = "move_object",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::move_object()
        )
    )]
    pub async fn move_object(
        &self,
        Parameters(input): Parameters<MoveObjectInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "move_object",
            OPERATION_MOVE_OBJECT,
            instance_id,
            move || input.to_params(),
            describe::move_object,
        )
        .await
    }

    /// オブジェクト名を変更する。name を省略するか null にすると標準名へ戻す。
    #[tool(
        name = "set_object_name",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_object_name()
        )
    )]
    pub async fn set_object_name(
        &self,
        Parameters(input): Parameters<SetObjectNameInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "set_object_name",
            OPERATION_SET_OBJECT_NAME,
            instance_id,
            move || input.to_params(),
            describe::set_object_name,
        )
        .await
    }

    /// effect の設定項目またはトラックバーの値を変更する。
    /// 書き込みを公開していない設定項目種別は item_type が data のものと、
    /// 種別を解釈できないものだけであり、その場合は unsupported_operation となる。
    /// 種別は get_object の item_type で確認できる。
    /// 設定項目の種別が値の形を受け付けない場合は invalid_argument となり、
    /// details.item_type に設定項目の種別が、details.value_kind に与えた値の形が入る。
    /// トラックバー以外の設定項目へ track を書いた場合もこれになる。
    /// 移動を持つトラックバーへ number や integer を書く要求は unsupported_operation
    /// となり details.reason は track_movement_present になる。書けば移動もその
    /// パラメータも消えるためであり、消したい場合は mode を null にした track を送る。
    /// details.current_value にホストが現在保持している値が入り、書き込みは発行されない。
    /// current_value はそのまま送り返せる形ではない。移動を書き戻すには、読み取った
    /// 値ではなく get_object が返す track の形で組み直す。
    /// 移動を持たないトラックバーへ track を書く要求は通り、新しく移動が付く。
    /// 書き込みは全ての種別で、対象を読み直してから設定値を読んで照合する。
    /// 要求した値が入っていなければ unsupported_operation となり details.reason は
    /// item_value_not_applied、details.observed_value に照合で読んだ設定値が入る。
    /// この失敗では書き込みは既に発行済みだが、設定項目は書き込み前の値へ戻す。
    /// 戻せたかは details.restored が名乗り、
    /// 戻せなかった場合だけ details.consistency_unknown が true になる。
    /// 戻せていれば selector はそのまま使え、対象を読み直す必要は無い。
    /// このとき details.retry_requires は none になる。
    /// observed_value は応答が返る時点の現在値ではなく、要求の代わりに送り直す値でもない。
    /// 要求した値がホストに受け付けられなかったと解し、受け付けられる値を選び直す。
    /// 選択肢から選ぶ種別（select・combo・mask・figure）で選択肢に無い値、登録されていない
    /// フォント名、書式の合わない色はいずれもこの失敗になる。
    /// 数値が値域を外れてクランプされた場合と、小数が項目の桁数へ丸められた場合も
    /// 同じ失敗になる。ホストが値を調整したことと拒否したことは区別できないため、
    /// 要求した値を得られていない点で同じ扱いにする。
    /// item_type が integer・scene・range の項目へ書ける整数には幅がある。
    /// 幅を外した要求は書き込みを発行せずに
    /// invalid_argument（argument_not_representable）となる。
    /// この幅は describe_effects の range には現れず、入力 schema が宣言する。
    /// track の params は個数も意味も移動方法ごとに決まる。受け付けられない params は
    /// 要求として受理され、書き込みも発行される。その先は移動方法で分かれる——
    /// 時間制御の変種では失敗として現れない。書いたとおりに保存され、
    /// 評価だけが既定へ倒れるためである。値域を外れた綴りも同じになる。
    /// それ以外の移動方法では item_value_not_applied になる。ホストが保存値を
    /// 既定値へ差し替えるか params ごと落とすためであり、照合がその差を見る。
    /// 評価がどうなったかは get_effect_item_values で確かめる。
    #[tool(
        name = "set_object_item",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_object_item()
        )
    )]
    pub async fn set_object_item(
        &self,
        Parameters(input): Parameters<SetObjectItemInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "set_object_item",
            OPERATION_SET_OBJECT_ITEM,
            instance_id,
            move || input.to_params(),
            describe::set_object_item,
        )
        .await
    }

    /// オブジェクトへ effect を付与する。
    /// effect_name には list_available_effects が返す名前を指定する。
    /// 登録されていない名前は unsupported_operation となる。
    /// 同じ要求を再送すると重複して付与し得る。付与によってオブジェクトの
    /// fingerprint が変わるため、同じ selector での再送は precondition_failed と
    /// なり防がれる。
    /// effect の増減は、同じオブジェクトが持つ他の effect の selector も無効にする。
    /// 応答が返すのは付与した effect の selector だけであるため、
    /// 兄弟 effect を続けて編集するには get_object を引き直す。
    #[tool(
        name = "add_effect",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::add_effect()
        )
    )]
    pub async fn add_effect(
        &self,
        Parameters(input): Parameters<AddEffectInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "add_effect",
            OPERATION_ADD_EFFECT,
            instance_id,
            move || input.to_params(),
            describe::add_effect,
        )
        .await
    }

    /// effect の有効・無効を変更する。
    /// 出力 item の有効・無効は変更できず unsupported_operation となる。
    /// 応答の effect には変更後に読み直した effect が入る。
    #[tool(
        name = "set_effect_enabled",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_effect_enabled()
        )
    )]
    pub async fn set_effect_enabled(
        &self,
        Parameters(input): Parameters<SetEffectEnabledInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "set_effect_enabled",
            OPERATION_SET_EFFECT_ENABLED,
            instance_id,
            move || input.to_params(),
            describe::set_effect_enabled,
        )
        .await
    }

    /// effect を effect 列の別の位置へ動かす。
    /// position は列全体での 0 始まりの位置であり、get_object の effects 配列の
    /// 添字と同じ数え方である。同名 effect の順序を表す effect_index とは別の値である。
    /// 順序を動かせるのはフィルタ効果だけであり、入力 item・出力 item は
    /// unsupported_operation（effect_not_movable）となる。
    /// position が effect の件数以上の場合は precondition_failed
    /// （effect_position_out_of_range）となり、変更は発行されない。
    /// 下限は振る舞いが違う。フィルタ効果は先頭に並ぶ入力 item・出力 item より
    /// 前へは動けず、そこを指した position は発行されたうえでホストが切り詰める。
    /// 結果は unsupported_operation（change_not_applied）であり、
    /// details.reported_position にホストが名乗った位置が入る。
    /// 切り詰めで列が動いた場合は元の並びへ戻す。details.restored が真なら列は
    /// 要求の前と同じであり、このとき details.retry_requires は none になる。
    /// 対象が既に下限に居て列が 1 件も動かなかった場合も details.restored は真になる。
    /// 列が動いていない失敗では要求に使った selector がそのまま通る。
    /// details.restored が偽なら戻せておらず details.consistency_unknown が立つ。
    /// 応答の effect には移動後に読み直した effect が入る。
    /// 成功して列の位置が変われば、要求に使った selector は使えなくなる——
    /// fingerprint が変わり、同名 effect があれば effect_index も入れ替わる。
    /// 続けて同じ effect を編集する場合は応答の effect.selector を使う。
    /// 移動は間にある effect の位置もずらすため、兄弟 effect を編集するには
    /// get_object を引き直す。
    #[tool(
        name = "move_effect",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::move_effect()
        )
    )]
    pub async fn move_effect(
        &self,
        Parameters(input): Parameters<MoveEffectInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "move_effect",
            OPERATION_MOVE_EFFECT,
            instance_id,
            move || input.to_params(),
            describe::move_effect,
        )
        .await
    }

    /// オブジェクトから effect を削除する。
    /// 対象が既に失われている場合は not_found となり、追加の変更は起きない。
    /// 応答は effect を返さない（常に null）。
    /// effect の増減は、同じオブジェクトが持つ他の effect の selector も無効にする。
    /// 消した effect だけでなく兄弟 effect も指し直せなくなるため、
    /// 続けて編集するには get_object を引き直す。
    #[tool(
        name = "delete_effect",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::delete_effect()
        )
    )]
    pub async fn delete_effect(
        &self,
        Parameters(input): Parameters<DeleteEffectInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "delete_effect",
            OPERATION_DELETE_EFFECT,
            instance_id,
            move || input.to_params(),
            describe::delete_effect,
        )
        .await
    }

    /// オブジェクトを削除する。
    /// 対象が既に失われている場合は not_found となり、追加の変更は起きない。
    /// 他の編集 tool と異なり、応答は対象を返さない。削除した対象の selector は
    /// 以後どの編集にも使えない。
    /// 対象のレイヤーがロックされている場合は precondition_failed（layer_locked）と
    /// なる。set_layer_state でロックを解除してから再実行する。
    #[tool(
        name = "delete_object",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::delete_object()
        )
    )]
    pub async fn delete_object(
        &self,
        Parameters(input): Parameters<DeleteObjectInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "delete_object",
            OPERATION_DELETE_OBJECT,
            instance_id,
            move || input.to_params(),
            describe::delete_object,
        )
        .await
    }

    /// オブジェクトへ中間点を追加し、区間を 1 つ増やす。
    /// frame は中間点を置くシーンの絶対フレーム番号であり、オブジェクト内の相対位置
    /// ではない。get_object が返した sections の値をそのまま基準に使える。
    /// 応答の sections は変更後の区間の一覧であり、get_object が返すものと同じ形である。
    /// 区間の番号と中間点の番号は 1 つずれる。sections[i] が区間番号 i であり、
    /// i が 1 以上のとき sections[i].start が i 番目の中間点のフレームである。
    /// sections[0].start はオブジェクトの開始フレームであって中間点ではないため、
    /// 区間 0 は delete_object_section でも move_object_section でも指定できない。
    /// sections の末尾の end はオブジェクトの終了フレームである。
    /// frame がオブジェクトの範囲外なら precondition_failed（frame_outside_object）、
    /// 既に区間の開始フレームなら precondition_failed（section_boundary_exists）となる。
    /// 同じ要求を再送しても中間点は重複しない。2 回目は section_boundary_exists で
    /// 落ち、状態は 1 回目と同じである。
    /// 対象のレイヤーがロックされている場合は precondition_failed（layer_locked）と
    /// なる。set_layer_state でロックを解除してから再実行する。
    #[tool(
        name = "create_object_section",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::create_object_section()
        )
    )]
    pub async fn create_object_section(
        &self,
        Parameters(input): Parameters<CreateObjectSectionInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "create_object_section",
            OPERATION_CREATE_OBJECT_SECTION,
            instance_id,
            move || input.to_params(),
            |result| describe::object_sections("中間点を追加しました", result),
        )
        .await
    }

    /// オブジェクトの中間点を 1 つ削除し、前後の区間を 1 つにまとめる。
    /// section に 0 を指定すると invalid_argument（section_index_out_of_range）となる。
    /// section が区間の数以上なら precondition_failed（section_index_out_of_range）となる。
    /// 同じ事実でも、常に誤りである 0 は invalid_argument、対象の現在の状態に
    /// よって決まる範囲外は precondition_failed になる。
    /// 削除した中間点の移動パラメータは失われ、create_object_section で同じ
    /// フレームへ中間点を戻しても元の値には戻らない。
    /// 応答の sections は変更後の区間の一覧であり、get_object が返すものと同じ形である。
    /// sections の末尾の end はオブジェクトの終了フレームである。
    /// 対象のレイヤーがロックされている場合は precondition_failed（layer_locked）と
    /// なる。set_layer_state でロックを解除してから再実行する。
    #[tool(
        name = "delete_object_section",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::delete_object_section()
        )
    )]
    pub async fn delete_object_section(
        &self,
        Parameters(input): Parameters<DeleteObjectSectionInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "delete_object_section",
            OPERATION_DELETE_OBJECT_SECTION,
            instance_id,
            move || input.to_params(),
            |result| describe::object_sections("中間点を削除しました", result),
        )
        .await
    }

    /// オブジェクトの中間点を別のフレームへ移す。
    /// frame は移動先のシーンの絶対フレーム番号であり、オブジェクト内の相対位置ではない。
    /// section に 0 を指定すると invalid_argument（section_index_out_of_range）となる。
    /// sections の末尾の end はオブジェクトの終了フレームである。
    /// 中間点は隣の中間点を追い越せない。移動できるのは sections[section-1].start より後、
    /// sections[section+1].start より前（無ければオブジェクトの終了フレームまで）であり、
    /// 外れると precondition_failed（section_move_crosses_boundary）となる。
    /// section が区間の数以上なら precondition_failed（section_index_out_of_range）となる。
    /// 応答の sections は変更後の区間の一覧であり、get_object が返すものと同じ形である。
    /// 対象のレイヤーがロックされている場合は precondition_failed（layer_locked）と
    /// なる。set_layer_state でロックを解除してから再実行する。
    #[tool(
        name = "move_object_section",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::move_object_section()
        )
    )]
    pub async fn move_object_section(
        &self,
        Parameters(input): Parameters<MoveObjectSectionInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "move_object_section",
            OPERATION_MOVE_OBJECT_SECTION,
            instance_id,
            move || input.to_params(),
            |result| describe::object_sections("中間点を移動しました", result),
        )
        .await
    }

    /// レイヤーの名前・表示・ロック状態を変更する。
    /// name と enabled と locked の 3 つ全てを省略した要求は受け付けない。
    /// name に空の名前を指定すると invalid_argument となる。標準名へ戻すには reset を指定する。
    /// レイヤーには fingerprint が無いため、読み取った時点から状態が変わっていても
    /// 検出できない。応答が返す layer には変更後に読み直した実際の状態が入るので、
    /// 意図どおりかはその値で確認する。
    /// レイヤーのロックが止める範囲は AviUtl2 が決めており、オブジェクトの削除と
    /// 時間軸上の移動にとどまらない。MCP では move_object と delete_object と
    /// create_object と create_object_section と delete_object_section と
    /// move_object_section が precondition_failed（layer_locked）になる。
    /// 設定値の変更や effect の増減は止めない。
    /// この tool 自身はロックの影響を受けない。ロックされたレイヤーでもロックを外せる。
    #[tool(
        name = "set_layer_state",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_layer_state()
        )
    )]
    pub async fn set_layer_state(
        &self,
        Parameters(input): Parameters<SetLayerStateInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "set_layer_state",
            OPERATION_SET_LAYER_STATE,
            instance_id,
            move || input.to_params(),
            describe::layer_state,
        )
        .await
    }

    /// BPM グリッドの一覧を置き換える。部分更新ではない。entries に指定した一覧が
    /// そのまま現在の一覧になり、指定しなかった要素は消える。変えたい要素だけを
    /// 差し替えるには、get_edit_info が返した grid_bpm を受け取り、その要素を書き換えて
    /// 全件を送る。一覧全体が置き換わるため、置き換え前の一覧を保持していなければ
    /// 同じ状態へは戻せない。
    /// この tool は他の編集 tool と異なり取り消し単位を作らない。実行後に取り消し
    /// 操作を行うと、グリッドではなく、その前に行った編集が取り消される。
    /// 置き換え前の一覧は取り消し操作でも戻らない。
    /// entries を空配列にするとグリッドが消える。指定できるのは 256 件までである。
    /// start が一覧の中で重複する要求は invalid_argument（duplicate_target）となる。
    /// 値が範囲外の要求は invalid_argument（grid_bpm_out_of_range）となる。
    /// tempo は単精度へ丸めた結果も 0 より大きい必要があり、極端に小さい値は
    /// 丸めると 0 になるため同じ理由で拒否される。
    /// beat が 32bit 符号付き整数に収まらない要求は
    /// invalid_argument（argument_not_representable）となる。
    /// start の昇順は求めない。並べ替えはホストが行う。
    /// 応答の entries には置き換え後に読み直した一覧が入る。ホストは tempo と offset を
    /// 単精度で受け取り並べ替えもするため、要求した値や順序と一致するとは限らない。
    /// 確かめるのは件数だけであり、件数が食い違うと unsupported_operation
    /// （change_not_applied）となる。
    #[tool(
        name = "set_grid_bpm",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_grid_bpm()
        )
    )]
    pub async fn set_grid_bpm(
        &self,
        Parameters(input): Parameters<SetGridBpmInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "set_grid_bpm",
            OPERATION_SET_GRID_BPM,
            instance_id,
            move || input.to_params(),
            describe::grid_bpm,
        )
        .await
    }

    /// この操作は取り消せない。AviUtl2 の取り消し操作ではシーン設定は元へ戻らず、
    /// 取り消しを行うとその前に行った編集が取り消される。応答の non_undoable は
    /// 常に true であり、このことを示す。
    /// この操作は apply_batch に含められない。sub-operation として指定した要求は
    /// invalid_argument となる。
    /// シーンの名前・解像度・サンプリングレートを変更する。変更は常に現在シーンへ
    /// 掛かり、非現在シーンを指定する手段は無い。
    /// name と size と sample_rate の 3 つ全てを省略した要求は受け付けない。
    /// name に空の名前は指定できず invalid_argument（empty）となる。オブジェクト名や
    /// レイヤー名と違い、シーン名には「標準へ戻す」が無く、名前を消す手段も無い。
    /// 解像度は render_frame が描ける大きさに収まる必要がある。width と height の積が
    /// 1 フレームの非圧縮 RGBA8 の上限（256 MiB）を超える要求は invalid_argument となる。
    /// フレームレートは変更できない。現在の値は get_current_scene が返す fps_rate と
    /// fps_scale で読める。
    /// シーンには fingerprint が無いため、読み取った時点から状態が変わっていても
    /// 検出できない。応答が返す scene には変更後に観測した実際の状態が入るので、
    /// 意図どおりかはその値で確認する。
    /// 解像度とサンプリングレートの反映値は編集と原子的に観測したものではない。観測は
    /// 編集の区間を抜けた後に行い、ホストが値を調整し得るため、要求した値と異なって
    /// いても失敗にはならない。応答の observed_after_edit がこれを示す。
    /// シーン名だけは編集の区間の内側で照合する。反映されていなければ
    /// unsupported_operation（change_not_applied）となり、解像度とサンプリングレートは
    /// 1 つも変更されない。
    /// シーン設定には 0 始まりの軸が無く、応答の値は UI の表示と同じ単位である。
    #[tool(
        name = "set_scene_settings",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_scene_settings()
        )
    )]
    pub async fn set_scene_settings(
        &self,
        Parameters(input): Parameters<SetSceneSettingsInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "set_scene_settings",
            OPERATION_SET_SCENE_SETTINGS,
            instance_id,
            move || input.to_params(),
            describe::scene_settings,
        )
        .await
    }

    /// どこを見て何を選んでいるかを変更する。cursor はカーソル位置、selected_range は
    /// フレーム範囲選択、focus はフォーカス対象、display はレイヤー編集の表示開始位置である。
    /// cursor と selected_range と focus と display の 4 つ全てを省略した要求は受け付けない。
    /// cursor と display はどちらも設定できる範囲へ調整されるため、要求した値が
    /// そのまま入るとは限らない。応答の cursor と display には調整後の値が入る。
    /// ただし調整の扱いは 2 つで違う。cursor はクランプされても applied に入る。
    /// 実際に入った位置は応答の cursor を読んで確かめる。
    /// display はクランプされると not_applied に入る。
    /// したがって display だけは applied を見れば要求どおりの位置か判別できる。
    /// display の反映可否は表示開始位置だけで判定する。応答が返す表示フレーム数と
    /// 表示レイヤー数は厳密な値ではなく、判定にも使えない。
    /// この tool は他の編集 tool と異なり取り消し単位を作らない。実行後に取り消し
    /// 操作を行うと、カーソルや選択範囲ではなく、その前に行った編集が取り消される。
    /// 応答が返す反映値は編集と原子的に観測したものではなく、ホストが範囲外の値を
    /// クランプした結果である。実際に適用できた項目は applied が、要求したが
    /// 適用できなかった項目は not_applied が示す。一部だけが適用されても応答は
    /// 成功であり、not_applied が空でなければ残りは反映されていない。
    #[tool(
        name = "set_selection",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_selection()
        )
    )]
    pub async fn set_selection(
        &self,
        Parameters(input): Parameters<SetSelectionInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "set_selection",
            OPERATION_SET_SELECTION,
            instance_id,
            move || input.to_params(),
            describe::selection_state,
        )
        .await
    }

    /// 複数の編集を 1 つの取り消し単位としてまとめて適用する。
    /// operations へ入れられるのは move_object と set_object_item の 2 種だけであり、
    /// 他の編集は対応する単独 tool を使う。件数は 1 件以上 100 件以下である。
    /// この呼び出し 1 回の全体が 1 つの取り消し単位になる。
    /// 1 つの batch の中では、同じ読み取り時点の selector をそのまま並べてよい。
    /// 単独 tool を連続して呼ぶ場合と異なり、先行する変更で後続の selector が
    /// 無効にならない。全対象を変更前にまとめて照合するためである。
    /// 配列順に適用し、宛先の空きは適用時点で確かめる。したがって先行する移動が
    /// 空けた場所を、後続の移動の宛先にできる。
    /// ただし 2 つのオブジェクトが互いの位置を交換する 2 件は通らない。1 件目を
    /// 適用する時点で相手がまだ宛先に居るためである。交換は空きレイヤーを
    /// 経由する 3 件に分けること。
    /// 同じ対象の同じ状態を 2 回変更する要求は受け付けない。同じオブジェクトの
    /// 2 回の移動と、同じ設定項目への 2 回の書き込みがこれに当たる。
    /// 途中で失敗した場合はそれまでに適用した変更を自動で巻き戻す。
    /// 全て戻せた場合はプロジェクトが要求の前と同じであり、
    /// details.retry_requires は止めた失敗そのものが決める。
    /// 失敗したときは details.failed_index が何番目で落ちたかを返す。
    /// オブジェクトの fingerprint が食い違った場合は details.failed_object が
    /// その対象の現在の状態も返すので、100 件を読み直さずにその 1 件だけを
    /// 差し替えて再要求できる。effect の fingerprint が食い違った場合は
    /// details.failed_object が付かないため、対象オブジェクトを読み直す。
    /// details.consistency_unknown が立っている場合は巻き戻しに失敗しており、
    /// プロジェクトが中途半端な状態の可能性がある。必ず読み直すこと。
    /// details.rolled_back_count は復旧の手掛かりであって被害の正確な計量ではない。
    /// 1 件の巻き戻し失敗が後続の巻き戻しを連鎖的に失敗させ得るため、実際に
    /// 壊れている件数を過大に見積もり得る。
    /// ロックされたレイヤーが妨げるのは move_object だけであり、
    /// precondition_failed（layer_locked）となる。設定値の変更はロックされた
    /// レイヤー上でも通る。解除は set_layer_state で行う。
    /// 大きなプロジェクトでは適用中に AviUtl2 の UI が数秒止まり得る。
    #[tool(
        name = "apply_batch",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::apply_batch()
        )
    )]
    pub async fn apply_batch(
        &self,
        Parameters(input): Parameters<ApplyBatchInput>,
    ) -> CallToolResult {
        let instance_id = input.instance_id.clone();
        self.run_operation(
            "apply_batch",
            OPERATION_APPLY_BATCH,
            instance_id,
            move || input.to_params(),
            describe::apply_batch,
        )
        .await
    }
}
