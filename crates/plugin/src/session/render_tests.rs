use super::*;
use aviutl2_mcp_core::{RenderFrameParams, RenderFrameResult, RequestBudgetKind};
use serde_json::json;
use std::sync::Mutex;

const EPOCH: &str = "9d0a5f4e-2f47-4a13-9a5e-1e2f3a4b5c6d";
const SCENE_ID: i32 = 0;
const FRAME: u32 = 42;

/// 引き渡し用ファイルの識別子。小文字 16 進 32 文字。
const HANDOFF_TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// 成果物の置き場。応答にも補助情報にも現れてはならない値の代表。
const ARTIFACT_PATH: &str = r"C:\Users\example\artifacts\frame.png";

/// レンダリングの実行口の代わりに定型データを返す実装。
///
/// 呼び出しを記録するため、受付判定や params の検証で弾かれた要求が実行口へ
/// 進んでいないことを確かめられる。
struct FakeRenderAdapter {
    calls: Mutex<Vec<&'static str>>,
    discarded: Mutex<Vec<String>>,
    /// 最初の呼び出しで返す失敗。
    failure: Mutex<Option<RenderError>>,
}

impl FakeRenderAdapter {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            discarded: Mutex::new(Vec::new()),
            failure: Mutex::new(None),
        }
    }

    /// 最初のレンダリングが指定の失敗を返す実行口を作る。
    fn failing(error: RenderError) -> Self {
        let adapter = Self::new();
        *adapter.failure.lock().unwrap() = Some(error);
        adapter
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    fn discarded(&self) -> Vec<String> {
        self.discarded.lock().unwrap().clone()
    }
}

impl RenderAdapter for FakeRenderAdapter {
    fn render_frame(&self, params: &RenderFrameParams) -> Result<RenderFrameResult, RenderError> {
        self.calls.lock().unwrap().push("render_frame");
        if let Some(error) = self.failure.lock().unwrap().take() {
            return Err(error);
        }
        Ok(RenderFrameResult {
            project_epoch: EPOCH.to_string(),
            project_revision: 3,
            scene_id: params.expected_scene_id,
            frame: params.frame,
            width: 320,
            height: 180,
            media_type: crate::render::ARTIFACT_MEDIA_TYPE.to_string(),
            byte_length: 4,
            sha256: format!("sha256:{}", "0".repeat(64)),
            handoff_token: HANDOFF_TOKEN.to_string(),
        })
    }

    fn discard_artifact(&self, handoff_token: &str) {
        self.discarded
            .lock()
            .unwrap()
            .push(handoff_token.to_string());
    }
}

/// 受け付けられる要求の params。
fn render_params() -> Value {
    json!({ "expected_scene_id": SCENE_ID, "frame": FRAME })
}

/// 期限内の判定。
fn within() -> RequestDeadline {
    RequestDeadline::Within(Instant::now() + Duration::from_secs(1))
}

#[test]
fn every_render_operation_is_routed_from_its_name() {
    for operation in RenderOperation::ALL {
        assert_eq!(
            classify_operation(operation.as_str()).unwrap(),
            Operation::Render(operation),
            "{} がレンダリングへ振り分けられていません",
            operation.as_str()
        );
    }
}

#[test]
fn a_render_within_the_deadline_reaches_the_adapter() {
    let adapter = FakeRenderAdapter::new();
    let result = execute_render(
        &adapter,
        &InstanceState::Ready,
        RenderOperation::RenderFrame,
        &render_params(),
        within(),
    )
    .expect("期限内のレンダリングが拒否されました");

    assert_eq!(adapter.calls(), vec!["render_frame"]);
    assert_eq!(result.scene_id, SCENE_ID);
    assert_eq!(result.frame, FRAME);
    assert_eq!(result.handoff_token, HANDOFF_TOKEN);
}

#[test]
fn render_params_are_decoded_before_the_state_and_the_deadline() {
    // 要求内容の誤りはライフサイクル状態にも期限にも依存しない。受付判定を
    // 先に通すと、解消しない誤りが再試行を促す host_busy として返る。期限
    // 判定を先に通すと、同じ誤りが再試行可能な timeout に化ける。**どちらの
    // 順序も塞ぐ。** 片方だけを見ていると、もう一方へ入れ替える変更が
    // 素通りする。
    for (state, deadline, order) in [
        (InstanceState::Starting, within(), "起動処理中"),
        (InstanceState::Ready, RequestDeadline::Exceeded, "期限超過"),
    ] {
        for params in [
            json!({ "expected_scene_id": SCENE_ID }),
            json!({ "expected_scene_id": SCENE_ID, "frame": FRAME, "future": 1 }),
            json!({ "expected_scene_id": SCENE_ID, "frame": -1 }),
            json!({ "expected_scene_id": SCENE_ID, "frame": u32::MAX }),
            json!({ "frame": FRAME }),
        ] {
            let adapter = FakeRenderAdapter::new();
            let error = execute_render(
                &adapter,
                &state,
                RenderOperation::RenderFrame,
                &params,
                deadline,
            )
            .unwrap_err();

            assert_eq!(
                error.code,
                ErrorCode::InvalidArgument,
                "{order} の {params} が要求内容の誤りとして返りませんでした"
            );
            assert!(adapter.calls().is_empty(), "{order}: {params}");
        }
    }
}

#[test]
fn a_starting_instance_rejects_a_well_formed_render() {
    let adapter = FakeRenderAdapter::new();
    let error = execute_render(
        &adapter,
        &InstanceState::Starting,
        RenderOperation::RenderFrame,
        &render_params(),
        within(),
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::HostBusy);
    assert!(error.retryable);
    assert!(adapter.calls().is_empty());
}

#[test]
fn an_expired_deadline_stops_the_render_before_it_starts() {
    let adapter = FakeRenderAdapter::new();
    let error = execute_render(
        &adapter,
        &InstanceState::Ready,
        RenderOperation::RenderFrame,
        &render_params(),
        RequestDeadline::Exceeded,
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(error.retryable);
    assert!(
        adapter.calls().is_empty(),
        "期限超過後にホストへタスクを投入しました"
    );
}

#[test]
fn a_render_timeout_never_warns_about_an_applied_change() {
    // レンダリングはプロジェクトを変更しない。変更の有無を伝えるキーを
    // 添えると、要求元は編集と同じ警戒（読み直してから再送）を要すると
    // 誤解する。
    let adapter = FakeRenderAdapter::new();
    let render = execute_render(
        &adapter,
        &InstanceState::Ready,
        RenderOperation::RenderFrame,
        &render_params(),
        RequestDeadline::Exceeded,
    )
    .unwrap_err();
    assert_eq!(render.code, ErrorCode::Timeout);
    assert_eq!(render.details.get("change_applied"), None);
    assert_eq!(render.details.get("mutation_origin"), None);

    // 完了を待てなかった失敗にも付かない。代わりに落ちた段を伝える。
    let waited = render_error(RenderError::WaitTimeout);
    assert_eq!(waited.code, ErrorCode::Timeout);
    assert_eq!(waited.details.get("change_applied"), None);
    assert_eq!(waited.details["render_stage"], json!("wait"));

    // 編集の同じ経路には付く。変更が入ったか判別できる側だけが名乗る。
    let edit = edit_timeout_before_execution();
    assert_eq!(edit.code, ErrorCode::Timeout);
    assert_eq!(edit.details["change_applied"], json!("no"));
}

#[test]
fn a_render_result_is_never_discarded_after_its_deadline() {
    // レンダリングの結果を捨てると引き渡し用ファイルが宙に浮く。受け取る
    // 側は識別子を得ていないため掃除できない。
    let now = Instant::now();
    assert_eq!(retained_send_deadline(now), now + write_timeout());

    // 読み取りは同じ状況で結果を捨てる。破棄経路をレンダリングへ
    // 再利用しないことが、この差として現れる。
    assert_eq!(
        decide_send(
            now,
            NOW_UNIX_MS,
            RequestDeadline::Within(now - Duration::from_millis(1)),
            None,
        ),
        SendDecision::Discard
    );
}

/// 実行口が返した成果物つきの結果。
fn rendered(adapter: &FakeRenderAdapter) -> RenderFrameResult {
    execute_render(
        adapter,
        &InstanceState::Ready,
        RenderOperation::RenderFrame,
        &render_params(),
        within(),
    )
    .expect("期限内のレンダリングが拒否されました")
}

#[test]
fn a_failed_send_returns_the_artifact_to_the_adapter() {
    let adapter = FakeRenderAdapter::new();

    // 送信できた成果物は受け取る側が所有する。消してはならない。
    deliver_render_response(&adapter, Ok(rendered(&adapter)), |outcome| {
        // 応答へ載るのは実行口が返した結果そのものである。
        assert_eq!(outcome.unwrap()["handoff_token"], json!(HANDOFF_TOKEN));
        Ok(())
    })
    .expect("送信できた応答が失敗として返りました");
    assert!(
        adapter.discarded().is_empty(),
        "送信できた成果物を消しました"
    );

    // 送信に失敗すれば受け取る側は識別子を持たない。ここで消す。
    deliver_render_response(&adapter, Ok(rendered(&adapter)), |_| {
        anyhow::bail!("応答の送信に失敗しました")
    })
    .expect_err("送信の失敗が握り潰されました");
    assert_eq!(adapter.discarded(), vec![HANDOFF_TOKEN.to_string()]);
}

#[test]
fn a_failed_send_of_a_failure_response_has_no_artifact_to_return() {
    // 失敗した要求は成果物を作っていない。消すものが無いのに実行口を
    // 呼ぶと、他の要求の成果物を消しかねない。
    let adapter = FakeRenderAdapter::new();
    let failure = render_error(RenderError::WaitTimeout);
    deliver_render_response(&adapter, Err(failure.clone()), |outcome| {
        assert_eq!(outcome.unwrap_err(), failure);
        anyhow::bail!("応答の送信に失敗しました")
    })
    .expect_err("送信の失敗が握り潰されました");
    assert!(adapter.discarded().is_empty());
}

#[test]
fn only_the_render_response_leaves_something_to_clean_up() {
    // レンダリングの結果は引き渡し用ファイルを 1 つ残す。だから送信に
    // 失敗したときの後始末が要る。
    let adapter = FakeRenderAdapter::new();
    assert!(!rendered(&adapter).handoff_token.is_empty());

    // 一括適用の結果は掃除すべきものを 1 つも残さない。変更は既に取り消し
    // 単位へ登録されており、応答を送れなくても差し戻せるものは無い。
    let outcome = serde_json::to_value(aviutl2_mcp_core::BatchOutcome {
        project_epoch: EPOCH.to_string(),
        project_revision: 1,
        results: Vec::new(),
    })
    .unwrap();
    assert_eq!(outcome.get("handoff_token"), None);
    assert!(!outcome.to_string().contains(HANDOFF_TOKEN));
}

/// レンダリングの失敗の分類と、その代表値を 1 つの宣言から作る。
///
/// **`RenderError` を直接並べない。** 失敗は複製できないため一覧は生成
/// 関数の列になり、生成関数の列は網羅性を型で縛れない。分類の列挙を挟むと、
/// [`RenderFailure::error`] の網羅 `match` が分類の追加でコンパイルを止める。
///
/// **一覧と生成を別々に書かない。** 分けると、一覧から 1 件落ちても生成側は
/// 動いたままに見える。同じ `RenderError` variant を 2 つの分類が指す場合、
/// 落ちた 1 件は variant の網羅を見るテストからも隠れてしまう。
macro_rules! render_failures {
    ($($variant:ident => $error:expr),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum RenderFailure {
            $($variant),+
        }

        impl RenderFailure {
            /// 全分類。宣言と同じ並びで、落とすことも増やすこともできない。
            const ALL: &'static [RenderFailure] = &[$(RenderFailure::$variant),+];

            /// 対応する失敗の代表値を作る。
            fn error(self) -> RenderError {
                match self {
                    $(RenderFailure::$variant => $error),+
                }
            }
        }
    };
}

render_failures! {
    Read => RenderError::Read(crate::read::ReadError::NotReady),
    ReadEditBlocked => RenderError::Read(crate::read::ReadError::EditBlocked {
        state: crate::read::EditState::Save,
    }),
    SceneMismatch => RenderError::SceneMismatch {
        expected: SCENE_ID,
        current: SCENE_ID + 1,
    },
    FrameOutOfRange => RenderError::FrameOutOfRange,
    FrameTooLarge => RenderError::FrameTooLarge,
    WaitTimeout => RenderError::WaitTimeout,
    ShuttingDown => RenderError::ShuttingDown,
    TooManyAbandoned => RenderError::TooManyAbandoned,
    InvalidBuffer => RenderError::InvalidBuffer {
        rule: crate::render::BufferRule::PitchTooSmall,
    },
    ArtifactEncode => RenderError::Artifact {
        stage: crate::render::ArtifactStage::Encode,
    },
    ArtifactWrite => RenderError::Artifact {
        stage: crate::render::ArtifactStage::Write,
    },
    Sdk => RenderError::Sdk {
        operation: "rendering_scene_video",
    },
    Panicked => RenderError::Panicked,
}

#[test]
fn every_render_error_variant_has_a_failure_case() {
    // 分類の網羅 match は `RenderError` の variant 追加を止めない。
    // variant を 1 つずつ名指しし、名前の集合が一致することを主張する。
    // ここが落ちたら、足した variant を [`RenderFailure`] へも足す。
    fn variant_name(error: &RenderError) -> &'static str {
        match error {
            RenderError::Read(_) => "Read",
            RenderError::SceneMismatch { .. } => "SceneMismatch",
            RenderError::FrameOutOfRange => "FrameOutOfRange",
            RenderError::FrameTooLarge => "FrameTooLarge",
            RenderError::WaitTimeout => "WaitTimeout",
            RenderError::ShuttingDown => "ShuttingDown",
            RenderError::TooManyAbandoned => "TooManyAbandoned",
            RenderError::InvalidBuffer { .. } => "InvalidBuffer",
            RenderError::Artifact { .. } => "Artifact",
            RenderError::Sdk { .. } => "Sdk",
            RenderError::Panicked => "Panicked",
        }
    }

    const VARIANTS: &[&str] = &[
        "Read",
        "SceneMismatch",
        "FrameOutOfRange",
        "FrameTooLarge",
        "WaitTimeout",
        "ShuttingDown",
        "TooManyAbandoned",
        "InvalidBuffer",
        "Artifact",
        "Sdk",
        "Panicked",
    ];

    let mut covered: Vec<&str> = RenderFailure::ALL
        .iter()
        .map(|failure| variant_name(&failure.error()))
        .collect();
    covered.sort_unstable();
    covered.dedup();

    let mut expected = VARIANTS.to_vec();
    expected.sort_unstable();
    assert_eq!(covered, expected, "代表値が全 variant を覆っていません");

    // variant の網羅だけでは足りない。`Read` のように内側の値で振る舞いが
    // 変わる variant は代表値を複数持つため、そのうち 1 件を落としても
    // 上の主張は通ってしまう。**観測結果の集合そのものを固定する。**
    let observed: Vec<(String, String, String)> = RenderFailure::ALL
        .iter()
        .map(|failure| {
            let error = failure.error();
            (
                error.error_code().to_string(),
                error.to_string(),
                error.details().to_string(),
            )
        })
        .collect();

    let mut unique = observed.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        observed.len(),
        "同じ観測結果を返す代表値が重複しています。増やしても網羅は増えません"
    );
    assert_eq!(
        observed.len(),
        13,
        "代表値の件数が変わりました。減らすなら、失われた観測結果が \
         他の代表値で覆われていることを確かめてください"
    );
}

#[test]
fn render_failures_keep_their_code_and_details() {
    for &failure in RenderFailure::ALL {
        let expected = failure.error();
        let adapter = FakeRenderAdapter::failing(failure.error());

        let error = execute_render(
            &adapter,
            &InstanceState::Ready,
            RenderOperation::RenderFrame,
            &render_params(),
            within(),
        )
        .unwrap_err();

        assert_eq!(error.code, expected.error_code(), "{expected}");
        assert_eq!(error.retryable, expected.retryable(), "{expected}");
        assert_eq!(error.message, expected.to_string(), "{expected}");
        assert_eq!(error.details, expected.details(), "{expected}");
    }
}

#[test]
fn render_failures_never_produce_cancelled() {
    // 受信から実行までの窓も保留キューも無いため、取り消しは生成しない。
    for &failure in RenderFailure::ALL {
        let adapter = FakeRenderAdapter::failing(failure.error());
        let error = execute_render(
            &adapter,
            &InstanceState::Ready,
            RenderOperation::RenderFrame,
            &render_params(),
            within(),
        )
        .unwrap_err();
        assert_ne!(error.code, ErrorCode::Cancelled, "{}", error.message);
    }

    for deadline in [RequestDeadline::Exceeded, within()] {
        for state in [
            InstanceState::Starting,
            InstanceState::Draining,
            InstanceState::Gone,
        ] {
            let adapter = FakeRenderAdapter::new();
            let error = execute_render(
                &adapter,
                &state,
                RenderOperation::RenderFrame,
                &render_params(),
                deadline,
            )
            .unwrap_err();
            assert_ne!(error.code, ErrorCode::Cancelled, "{state}");
        }
    }
}

#[test]
fn render_failures_only_use_allowed_details_keys() {
    // 補助情報のキーはここで列挙したものに限る。新しいキーを足す際は、
    // 引き渡し用の識別子・パス・画像でないことを確かめた上で追加する。
    const ALLOWED: &[&str] = &[
        "retry_after_ms",
        "edit_state",
        "expected_scene_id",
        "current_scene_id",
        "mismatch",
        "reason",
        "render_stage",
        "sdk_operation",
        "retry_requires",
    ];
    for &failure in RenderFailure::ALL {
        let adapter = FakeRenderAdapter::failing(failure.error());
        let error = execute_render(
            &adapter,
            &InstanceState::Ready,
            RenderOperation::RenderFrame,
            &render_params(),
            within(),
        )
        .unwrap_err();

        for key in error.details.as_object().expect("補助情報は object").keys() {
            assert!(
                ALLOWED.contains(&key.as_str()),
                "{} の補助情報に未許可のキー {key} が含まれています",
                error.message
            );
        }
    }
}

#[test]
fn neither_the_handoff_token_nor_a_path_reaches_a_failure_response() {
    // 引き渡し用の識別子を渡せば、それだけで成果物の在り処が分かる。画像
    // には利用者のプロジェクトの内容が写る。どちらも失敗の応答へ出さない。
    let mut documents = Vec::new();
    for &failure in RenderFailure::ALL {
        let adapter = FakeRenderAdapter::failing(failure.error());
        let error = execute_render(
            &adapter,
            &InstanceState::Ready,
            RenderOperation::RenderFrame,
            &render_params(),
            within(),
        )
        .unwrap_err();
        documents.push(serde_json::to_string(&error).unwrap());
    }
    documents.push(
        serde_json::to_string(
            &execute_render(
                &FakeRenderAdapter::new(),
                &InstanceState::Ready,
                RenderOperation::RenderFrame,
                &render_params(),
                RequestDeadline::Exceeded,
            )
            .unwrap_err(),
        )
        .unwrap(),
    );

    for document in documents {
        let lowered = document.to_lowercase();
        for forbidden in [HANDOFF_TOKEN, ARTIFACT_PATH, ".png", r"c:\", "token", "0x"] {
            assert!(
                !lowered.contains(&forbidden.to_lowercase()),
                "{forbidden} が応答に含まれます: {document}"
            );
        }
    }
}

#[test]
fn a_render_is_given_its_own_execution_budget() {
    assert_eq!(
        execution_timeout(Operation::Render(RenderOperation::RenderFrame)),
        render_timeout()
    );
    // 最も短い予算へ落ちると、投入した瞬間に予算が尽きる。
    assert_ne!(render_timeout(), read_timeout());
    assert_ne!(render_timeout(), edit_timeout());

    // 実行の上限は、実行口が持つ 2 つの段の取り分をどちらも覆う。片方だけを
    // 数えると、もう一方の段へ入った時点で既に上限を超えている。
    assert!(render_timeout() > budgets().plugin_render_wait());
    assert!(render_timeout() > budgets().plugin_render_artifact());

    // レンダリングが実行の上限まで走っても、応答送信と、要求元が応答を
    // 受けてから行う成果物の引き取りの持ち時間が要求フェーズ予算の内側に
    // 残る。**引き取りの段を数え忘れると、どの層の期限にも捕まらないまま
    // 予算を超えてから成功する経路ができる。**
    let render = render_timeout();
    let write = write_timeout();
    let server = ScaledBudgets::unscaled();
    let ingest = server.server_artifact_ingest();
    let render_request = server.server_request_phase(RequestBudgetKind::Render);
    assert!(
        render + write + server.transport_headroom() + ingest <= render_request,
        "レンダリング {render:?} と送信 {write:?} と引き取り {ingest:?} が要求フェーズ予算 {render_request:?} に収まらない"
    );
}

/// 期限判定の基準時刻。読み取り側のテストと同じ値を用いる。
const NOW_UNIX_MS: i64 = 1_785_144_000_000;
