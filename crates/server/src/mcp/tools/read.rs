//! 読み取り tool。

use crate::mcp::describe;
use crate::mcp::input::{
    DescribeEffectsInput, GetEffectItemValuesInput, GetObjectInput, GetSelectionInput,
    InstanceInput, ListAvailableEffectsInput, ListFontsInput, ListLayersInput, ListModulesInput,
    ListObjectAliasesInput, ListObjectsInput, ListPalettesInput, parse_instance_id,
};
use crate::mcp::server::{
    AviUtl2McpServer, CallBudgets, ToolSuccess, request_operation, to_structured,
};
use aviutl2_mcp_core::{
    DescribeEffectsResult, EditInfo, EffectItemValues, GetCurrentSceneParams,
    GetCurrentSceneResult, GetEditInfoParams, ListAvailableEffectsResult, ListFontsResult,
    ListLayersResult, ListModulesResult, ListObjectAliasesResult, ListObjectsResult,
    ListPalettesResult, OPERATION_DESCRIBE_EFFECTS, OPERATION_GET_CURRENT_SCENE,
    OPERATION_GET_EDIT_INFO, OPERATION_GET_EFFECT_ITEM_VALUES, OPERATION_GET_OBJECT,
    OPERATION_GET_SELECTION, OPERATION_LIST_AVAILABLE_EFFECTS, OPERATION_LIST_FONTS,
    OPERATION_LIST_LAYERS, OPERATION_LIST_MODULES, OPERATION_LIST_OBJECT_ALIASES,
    OPERATION_LIST_OBJECTS, OPERATION_LIST_PALETTES, ObjectDetail, SelectionSnapshot,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

#[tool_router(router = read_tools_router, vis = "pub(in crate::mcp)")]
impl AviUtl2McpServer {
    /// 現在の編集情報（シーン・カーソル・表示範囲・選択範囲・revision）を取得する。
    #[tool(
        name = "get_edit_info",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::edit_info()
        )
    )]
    pub async fn get_edit_info(
        &self,
        Parameters(input): Parameters<InstanceInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("get_edit_info", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let result: EditInfo = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_GET_EDIT_INFO,
                &GetEditInfoParams {},
            )?;
            Ok(ToolSuccess {
                text: describe::edit_info(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// 現在シーンの情報と取得時点の project_revision を取得する。
    #[tool(
        name = "get_current_scene",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::current_scene()
        )
    )]
    pub async fn get_current_scene(
        &self,
        Parameters(input): Parameters<InstanceInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("get_current_scene", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let result: GetCurrentSceneResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_GET_CURRENT_SCENE,
                &GetCurrentSceneParams {},
            )?;
            Ok(ToolSuccess {
                text: describe::current_scene(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// 現在シーンのレイヤーを列挙する。
    #[tool(
        name = "list_layers",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_layers()
        )
    )]
    pub async fn list_layers(
        &self,
        Parameters(input): Parameters<ListLayersInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_layers", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListLayersResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_LIST_LAYERS,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::layers(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// 現在シーンのオブジェクトを列挙する。
    #[tool(
        name = "list_objects",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_objects()
        )
    )]
    pub async fn list_objects(
        &self,
        Parameters(input): Parameters<ListObjectsInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_objects", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListObjectsResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_LIST_OBJECTS,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::objects(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// オブジェクトの詳細（alias・中間点区間・effect・revision）を取得する。
    /// effect の locked は出力項目（標準描画等）については実態を反映せず、
    /// 常に false になる。ロックは入力項目と出力項目をまとめた単位で掛かる。
    #[tool(
        name = "get_object",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::object_detail()
        )
    )]
    pub async fn get_object(
        &self,
        Parameters(input): Parameters<GetObjectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("get_object", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ObjectDetail = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_GET_OBJECT,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::object_detail(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// いま選ばれているオブジェクトを取得する。set_selection の読み取り側である。
    /// focus と selected は別物である。
    /// focus はオブジェクト設定ウィンドウで選択されている 1 件、
    /// selected はタイムライン上で選択されている一覧であり、両者は一致しない。
    /// focus_section は focus の区間番号であり、区間番号 i は
    /// get_object が返す sections[i] を指す。focus が null のとき focus_section も null である。
    /// selected は layer 番号・frame_start の昇順で並び、list_objects と同じ並びである。
    /// ページ指定が掛かるのは selected だけであり、focus には掛からない。
    /// 編集カーソルとフレーム範囲選択は返さない。どちらも get_edit_info が返す。
    #[tool(
        name = "get_selection",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::get_selection()
        )
    )]
    pub async fn get_selection(
        &self,
        Parameters(input): Parameters<GetSelectionInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("get_selection", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: SelectionSnapshot = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_GET_SELECTION,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::selection(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// インスタンスが利用できる effect の一覧を取得する。
    /// 1 件につき名前・種別・対応フラグ・設定項目の数・説明を返す。
    /// description はホストが同梱する説明であり、持たない effect は null になる。空欄を推測で補わない。
    /// 設定項目の名前は返さない。対象へ付与したあと get_object を呼べば、項目名が現在値付きで得られる。
    #[tool(
        name = "list_available_effects",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_available_effects()
        )
    )]
    pub async fn list_available_effects(
        &self,
        Parameters(input): Parameters<ListAvailableEffectsInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_available_effects", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListAvailableEffectsResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_LIST_AVAILABLE_EFFECTS,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::available_effects(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// 名前で指定した effect の中身を取得する。
    /// effect_names には list_available_effects が返す名前を 1〜10 件指定する。
    /// 1 件につき name・description・items（name / item_type / description / choices / range / group）を返す。
    /// 設定項目の一覧はホストの列挙から得るため、必ず実際の effect と一致する。
    /// description はホストが同梱する説明であり、持たない effect と持たない項目は null になる。
    /// 説明を持たない effect は多く、とくにフィルタ効果はほとんどが null である。
    /// 空欄を推測で補わない。名前が似ている effect の使い分けは、説明ではなく
    /// items の顔ぶれで判断する。
    /// choices は選択肢の候補（values と source: builtin_table / sidecar）、range は値域と
    /// 小数桁（min・max・decimals と source）である。持たない項目は null になり、
    /// range の 3 つの値は測れた側だけが載るため個別に null になる。
    /// どちらもヒントであってゲートではない。候補に無い値でも書き込みは通り、
    /// 値域を外れる値でも書き込みは通る。候補に在る値が必ず通るとも限らない。
    /// 可否を決めるのはホストである。
    /// range は書き込む値に掛かるヒントであり、評価値の上下界ではない。
    /// 移動方法によっては、区間の境界へ書いた値が値域の内側でも、
    /// 途中のフレームの評価値が値域の外へ出る。
    /// group は設定項目が属するグループ（index と item_names）であり、座標の X / Y / Z の
    /// ように 1 つの組を成す項目を示す。属さない項目は null になる。
    /// このグループは名前を持たない。get_effect_item_values の text が示す group=<名前> は
    /// トラックバーのグループ名であり、別のものである。
    /// グループを引けなかった場合は要求全体が失敗する。null が返るのは属さない項目だけである。
    /// 登録されていない名前は not_found に並び、その名前だけが落ちる。
    /// 要求全体は失敗しないため、effects に無い名前は not_found を必ず確認すること。
    /// not_found に出た名前は綴りが違うだけであり、設定項目を持たない effect ではない。
    /// 設定項目の現在値は返さない。対象へ付与したあと get_object を呼べば現在値が得られる。
    /// ページ指定を持たない。返すのは指定した名前の分だけであり、続きのページという
    /// 概念が無いためである。
    #[tool(
        name = "describe_effects",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::describe_effects()
        )
    )]
    pub async fn describe_effects(
        &self,
        Parameters(input): Parameters<DescribeEffectsInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("describe_effects", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: DescribeEffectsResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_DESCRIBE_EFFECTS,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::effect_descriptions(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// インスタンスが利用できるフォント名の一覧を取得する。
    /// いずれも font 種別の設定項目へそのまま指定できる名前である。
    /// 名前による絞り込みは持たない。total_count で全体の件数が分かる。
    #[tool(
        name = "list_fonts",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_fonts()
        )
    )]
    pub async fn list_fonts(
        &self,
        Parameters(input): Parameters<ListFontsInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_fonts", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListFontsResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_LIST_FONTS,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::fonts(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// インスタンスが利用できるパレットの一覧と、各パレットの色を取得する。
    /// colors は常に 64 件であり、a は常に 255 である。
    /// つまりパレットは透明度の情報を持たない。
    /// current は現在のパレット名であり、ラベル付きの場合は [ラベル名.パレット名] の形式になる。
    /// 取得できない場合は null となるが、一覧はそのまま返る。
    /// 色を読み取れなかったパレットは一覧から除かれる。
    /// total_count から引かれるのは本ページで落とした分だけであり、
    /// 落ちたページとそうでないページで値が違い得る。全体の件数として扱わないこと。
    /// ページ内のすべてが落ちると items が空のまま has_more が true になり得る。
    /// 反復は items が空になったことではなく has_more と next_offset で終端すること。
    #[tool(
        name = "list_palettes",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_palettes()
        )
    )]
    pub async fn list_palettes(
        &self,
        Parameters(input): Parameters<ListPalettesInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_palettes", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListPalettesResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_LIST_PALETTES,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::palettes(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// インスタンスへ登録されているスクリプトとプラグインの一覧を取得する。
    /// information はホストが利用者へ表示する説明文である。
    /// 一覧には既知の 9 種別だけが現れる。
    /// 一覧に現れるのは後から登録されたものである。AviUtl2 に同梱されている
    /// スクリプトは、種別を解釈できても一覧に現れない。ある種別が 0 件である
    /// ことは、その種別の機能がインスタンスに無いことを意味しない。
    #[tool(
        name = "list_modules",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_modules()
        )
    )]
    pub async fn list_modules(
        &self,
        Parameters(input): Parameters<ListModulesInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_modules", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListModulesResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_LIST_MODULES,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::modules(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// インスタンスへ登録されているオブジェクトエイリアスの一覧を取得する。
    /// name はエイリアスの名前であり、create_object の alias_name へそのまま渡す値である。
    /// 一覧に出た名前は必ず作成できる。逆は保証しない。
    /// エイリアスの中身は返さない。返すのは name・label・object_count・effects だけである。
    /// label は AviUtl2 の UI 状態ファイル由来であり、欠けることがあり、
    /// 実行中の表示と一致しないことがある。
    /// label は識別子ではなく、複数のエイリアスが同じ label を共有し得る。
    /// 読み取れなかったエイリアスは一覧から除かれる。
    /// total_count から引かれるのは本ページで落とした分だけであり、
    /// 落ちたページとそうでないページで値が違い得る。全体の件数として扱わないこと。
    /// ページ内のすべてが落ちると items が空のまま has_more が true になり得る。
    /// 反復は items が空になったことではなく has_more と next_offset で終端すること。
    /// エイリアスの登録・削除・編集は AviUtl2 の UI で行う。この server は読み取りだけを提供する。
    /// AviUtl2 のデータディレクトリを解決できない環境では unsupported_operation となる。
    #[tool(
        name = "list_object_aliases",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_object_aliases()
        )
    )]
    pub async fn list_object_aliases(
        &self,
        Parameters(input): Parameters<ListObjectAliasesInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_object_aliases", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListObjectAliasesResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_LIST_OBJECT_ALIASES,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::object_aliases(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// effect の設定項目を、指定したフレームで評価した値を取得する。
    /// frames は get_object が返した frame_start / frame_end と同じ座標であり、
    /// オブジェクトの範囲外を指定すると precondition_failed（frame_out_of_range）となる。
    /// frames に小数を指定するとフレーム間の位置を指し、中間点・加減速・時間制御を
    /// 含む補間後の値が返る。トラックバー項目は小数部をそのまま使い、
    /// チェックボックス項目は整数部を使う。
    /// items を省略すると effect のトラックバー項目とチェックボックス項目すべてが
    /// 対象になり、上限を超えた分は打ち切られて truncated が true になる。
    /// items に指定した名前が effect に無ければ not_found（target_missing）、
    /// 名前はあるが評価できない種別なら unsupported_operation（item_not_evaluatable）となる。
    /// トラックバーグループの count はグループのトラック数、item_names は所属アイテム名で
    /// あり、両者の件数は一致しない場合がある。
    /// 各項目の values は frames と同じ長さ・同じ順序で並ぶ。
    #[tool(
        name = "get_effect_item_values",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::effect_item_values()
        )
    )]
    pub async fn get_effect_item_values(
        &self,
        Parameters(input): Parameters<GetEffectItemValuesInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("get_effect_item_values", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EffectItemValues = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_GET_EFFECT_ITEM_VALUES,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::effect_item_values(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }
}
