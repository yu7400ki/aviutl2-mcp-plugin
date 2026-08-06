//! 登録済みオブジェクトエイリアスの受け入れ規則と一覧。
//!
//! AviUtl2 のデータディレクトリ配下の `Alias\<名前>.object` を読む。SDK は
//! 1 度も呼ばず、ファイルシステムとパーサだけで完結する。
//!
//! 受け入れ規則は 1 つの関数へ閉じる。一覧の除外と作成の拒否が同じ戻り値を
//! 見ることで、「一覧に載る ⇒ 作成できる」が 1 件ずつについて成り立つ。
//! どちらか一方にだけ条件を足すことはできない。
//!
//! ディレクトリの解決と、解決した先を使う経路は分ける。解決は plugin の
//! 生存期間中に 1 度だけ行い、受け入れ規則と一覧はディレクトリを引数で受け取る。

use aviutl2::alias::Table;
use aviutl2_mcp_core::{
    ErrorCode, ListObjectAliasesResult, MAX_ALIAS_BYTES, ObjectAliasSummary, PageWindow,
    TextSyntaxError, take_window, validate_alias, validate_object_alias_name,
};
use serde_json::{Value, json};
use std::fs::File;
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::read::adapter::dropped_from_page;

/// エイリアスを収めたディレクトリの名前。
const ALIAS_DIRECTORY: &str = "Alias";

/// エイリアスファイルの拡張子。
const ALIAS_EXTENSION: &str = "object";

/// UI 状態ファイルの名前。
const HISTORY_FILE: &str = "history.ini";

/// UI 状態ファイルの中で、エイリアスの項目が並ぶ節。
const HISTORY_ALIAS_SECTION: &str = "Effect.object";

/// UI ラベルを持つキーの名前。
const LABEL_KEY: &str = "label";

/// effect 名を持つキーの名前。
///
/// `.` を含むが節の入れ子ではなく 1 つの値キーである。
const EFFECT_NAME_KEY: &str = "effect.name";

/// UI 状態ファイルとして読み込む最大バイト数。
///
/// この経路の費用は要求の内容で決まらないため、予算では守れない。上限は
/// 要求元が動かせない。
pub const MAX_HISTORY_INI_BYTES: u64 = 4 * 1024 * 1024;

/// AviUtl2 のデータディレクトリを解決できないことを表す名前。
pub const REASON_ALIAS_DIRECTORY_UNAVAILABLE: &str = "alias_directory_unavailable";

/// エイリアスを表として解釈できないことを表す名前。
pub const REASON_ALIAS_NOT_PARSABLE: &str = "alias_not_parsable";

/// エイリアスが effect を 1 つも含まないことを表す名前。
pub const REASON_ALIAS_WITHOUT_EFFECT: &str = "alias_without_effect";

/// 長さの上限を超えたことを表す名前。
///
/// 同じ事実を表す失敗は種別が別でも同じ名前を名乗る。
const REASON_TOO_LONG: &str = "too_long";

/// 節の入れ子として受け付ける深さの上限。
///
/// パーサが返す表は入れ子を再帰的に解放する。深い入れ子はスタックを使い切って
/// プロセスごと落とし、**スタックの枯渇は捕捉層では受け止められない。** 実測
/// では 20 KB 程度の入力でも深さ 10,000 で落ちる。
///
/// 実機が保存する形式の深さは `[Object.0]` の 2、UI 状態ファイルの
/// `[Effect.object.<名前>]` でも 3 である。上限はそれらを十分に上回る位置へ
/// 置く。
const MAX_SECTION_DEPTH: usize = 64;

/// 解決した AviUtl2 のデータディレクトリ。
///
/// 値は plugin の生存期間中に変わらず、取得のたびにロックと文字列の複製を
/// 伴うため 1 度だけ確定させる。
static DATA_DIRECTORY: OnceLock<Option<PathBuf>> = OnceLock::new();

/// panic を捕捉しながらデータディレクトリを解決する。
///
/// 上流には設定ハンドルが初期化済みかを問い合わせる手段が無く、未初期化の
/// 取得は panic で打ち切られる。捕捉しなければ「この AviUtl2 では機能が
/// 使えない」という失敗が「想定外の内部失敗」へ畳まれる。要求元に取れる手が
/// 無い点は同じでも、他の機能が正常に動くことは応答から読めなくなる。
fn resolve_data_directory(resolve: impl FnOnce() -> PathBuf) -> Option<PathBuf> {
    catch_unwind(AssertUnwindSafe(resolve)).ok()
}

/// AviUtl2 のデータディレクトリを返す。解決できなければ `None`。
///
/// 解決は初回の要求で 1 度だけ行う。エイリアス機能が使えないことは plugin が
/// 起動できない理由ではないため、初期化時には解決しない。
///
/// 解決した値はログへ 1 度だけ記録する。応答には載せない。
pub fn data_directory() -> Option<&'static Path> {
    DATA_DIRECTORY
        .get_or_init(|| {
            let resolved = resolve_data_directory(aviutl2::config::app_data_path);
            match &resolved {
                Some(path) => {
                    tracing::info!("AviUtl2 のデータディレクトリ: {}", path.display());
                }
                None => {
                    tracing::info!("AviUtl2 のデータディレクトリを解決できませんでした");
                }
            }
            resolved
        })
        .as_deref()
}

/// パース結果から導いたエイリアスの要約。
///
/// 応答へ載せるのはこれだけである。設定値も生テキストも持たない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasStructure {
    /// 作られるオブジェクト数。形式を判別できなければ `None`。
    pub object_count: Option<u32>,
    /// 含まれる effect 名の並び。出現順で、重複を保つ。
    pub effects: Vec<String>,
}

/// 受け入れ規則を通ったエイリアス。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedAlias {
    /// ファイル名（拡張子を除いたもの）。
    pub name: String,
    /// 読み取った生バイト列。SDK へはこれをそのまま渡す。
    ///
    /// パースは検証にのみ使い、書き戻さない。書き戻すと改行・空行・重複キーが
    /// 保存されず、同じ対象が版によって違うバイト列になる。
    pub raw: String,
    /// 応答へ載せる要約。
    pub summary: AliasStructure,
}

/// 受け入れ規則で落ちた条件。
///
/// 判定は費用の順（名前 → 存在 → 大きさと符号化 → 構造）に行い、最初に落ちた
/// 条件が失敗の種別を決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AliasRejection {
    /// 名前が構文規則を通らない。
    #[error("エイリアス名を指定できません: {0}")]
    ForbiddenName(TextSyntaxError),
    /// ファイルが存在しない、または読めない。
    #[error("指定された名前のエイリアスがありません")]
    NotFound,
    /// 大きさが上限を超えている。
    #[error("エイリアスが大きすぎます (上限 {MAX_ALIAS_BYTES} バイト)")]
    TooLarge,
    /// UTF-8 として解釈できない、NUL を含む、または表としてパースできない。
    #[error("エイリアスを解釈できません")]
    NotParsable,
    /// パースはできるが effect を 1 つも含まない。
    #[error("エイリアスが effect を 1 つも含みません")]
    WithoutEffect,
}

impl AliasRejection {
    /// 応答へ載せるエラーコードを返す。
    ///
    /// 不在だけが [`ErrorCode::NotFound`] であり、残りは要求元が指定した名前が
    /// 使えないファイルを指していることを述べる。復旧の手はいずれも「別の名前を
    /// 指定する」であり、要求内容の訂正に当たる。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            AliasRejection::NotFound => ErrorCode::NotFound,
            AliasRejection::ForbiddenName(_)
            | AliasRejection::TooLarge
            | AliasRejection::NotParsable
            | AliasRejection::WithoutEffect => ErrorCode::InvalidArgument,
        }
    }

    /// 失敗の種別を表す機械可読な名前を返す。
    ///
    /// 不在は名前を持たない。コードそのものが失敗を述べており、添えても分岐が
    /// 増えない。
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            AliasRejection::ForbiddenName(source) => Some(source.reason()),
            AliasRejection::NotFound => None,
            AliasRejection::TooLarge => Some(REASON_TOO_LONG),
            AliasRejection::NotParsable => Some(REASON_ALIAS_NOT_PARSABLE),
            AliasRejection::WithoutEffect => Some(REASON_ALIAS_WITHOUT_EFFECT),
        }
    }

    /// 応答へ載せる補助情報を組み立てる。
    ///
    /// 含めるのは失敗の種別名だけである。名前もファイルの内容も反響させない。
    pub fn details(&self) -> Value {
        match self.reason() {
            Some(reason) => json!({ "reason": reason }),
            None => json!({}),
        }
    }
}

/// 名前で指定されたエイリアスを受け入れられない理由。
///
/// 受け入れ規則そのものと、規則を掛ける相手が居ないことを分ける。前者は要求
/// 内容の誤りであり、後者は要求が正しくてもこの AviUtl2 では機能が使えないこと
/// を述べる。[`AliasRejection`] は 4 条件だけを表し、環境の事実を混ぜない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AliasAdmissionError {
    /// AviUtl2 のデータディレクトリを解決できない。
    #[error("AviUtl2 のデータディレクトリを解決できません")]
    DirectoryUnavailable,
    /// 受け入れ規則で落ちた。
    #[error(transparent)]
    Rejected(#[from] AliasRejection),
}

/// エイリアスを収めたディレクトリ。
///
/// **デバイス名の置換が起きない形（`\\?\` 前置）へ正規化済みであることを型が
/// 保証する。** 正規化はディレクトリ 1 つにつき 1 度だけ行う。窓の 1 件ごとに
/// 行うと、1 要求で開くハンドルの数が [`crate::read::ReadAdapter`] の定める
/// 上限（窓の大きさ ＋ 1）の 2 倍になる。
///
/// 保証を型で運ぶのは、組み立てる側が忘れられないようにするためである。パスを
/// 受け取る形にすると、呼び出しごとに正規化したかどうかが分かれる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasDirectory(PathBuf);

impl AliasDirectory {
    /// データディレクトリ配下のエイリアスディレクトリを正規化して得る。
    ///
    /// 存在しなければ `None` を返す。不在は「ディレクトリが違う」ことも「まだ
    /// 1 つも登録していない」ことも意味し、我々には区別できない。
    pub fn resolve(data_dir: &Path) -> Option<Self> {
        std::fs::canonicalize(data_dir.join(ALIAS_DIRECTORY))
            .ok()
            .map(Self)
    }

    /// 直下のエイリアスファイルのパスを組み立てる。
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(format!("{name}.{ALIAS_EXTENSION}"))
    }

    /// 正規化済みのディレクトリそのものを返す。
    fn path(&self) -> &Path {
        &self.0
    }
}

/// エイリアス名から、受け入れた結果か、落ちた条件を返す。
///
/// 判定は次の順で行う。
///
/// 1. 名前が構文規則を通る（ファイルを開かない）
/// 2. ファイルが存在し、読める
/// 3. 大きさが [`MAX_ALIAS_BYTES`] 以下で、UTF-8 として解釈でき、NUL を含まない
/// 4. 表としてパースでき、effect 名を 1 つ以上含む
///
/// `dir` はエイリアスを収めたディレクトリである。名前の判定はここを通る前に
/// 済んでおり、連結してから判定する形にはしない。
pub fn admit_alias(dir: &AliasDirectory, name: &str) -> Result<AdmittedAlias, AliasRejection> {
    validate_object_alias_name(name).map_err(AliasRejection::ForbiddenName)?;
    admit_named_file(dir, name)
}

/// データディレクトリの解決結果を起点に受け入れ規則を通す。
///
/// 一覧は窓の 1 件ごとに [`admit_alias`] を呼ぶため、正規化したディレクトリを
/// 1 度だけ組み立てて持ち回る。1 件だけを見る作成の経路にはその持ち回りが無く、
/// 組み立てまで含めてここが引き受ける。
///
/// **判定は名前の規則から始める。** 規則はファイルもディレクトリも要さずに
/// 決まり、費用の順で先に来る。データディレクトリの不在を先に名乗ると、名前が
/// 誤っている要求へ「この AviUtl2 では機能が使えない」と答えることになり、
/// 要求元は直せる誤りを直せないものとして読む。
///
/// エイリアスディレクトリが無い場合は、その名前のファイルも無い。
pub fn admit_alias_in(
    data_dir: Option<&Path>,
    name: &str,
) -> Result<AdmittedAlias, AliasAdmissionError> {
    validate_object_alias_name(name).map_err(AliasRejection::ForbiddenName)?;
    let data_dir = data_dir.ok_or(AliasAdmissionError::DirectoryUnavailable)?;
    let dir = AliasDirectory::resolve(data_dir).ok_or(AliasRejection::NotFound)?;
    Ok(admit_named_file(&dir, name)?)
}

/// 名前の規則を通った後の、条件 2 以降を判定する。
fn admit_named_file(dir: &AliasDirectory, name: &str) -> Result<AdmittedAlias, AliasRejection> {
    let raw = read_alias(dir, name)?;
    let table: Table = parse_table(&raw).ok_or(AliasRejection::NotParsable)?;
    let summary = summarize(&table);
    if summary.effects.is_empty() {
        return Err(AliasRejection::WithoutEffect);
    }
    Ok(AdmittedAlias {
        name: name.to_string(),
        raw,
        summary,
    })
}

/// エイリアスファイルを読み、大きさと符号化を確かめる。
///
/// `CON` や `NUL` といった Windows の予約デバイス名は禁止文字の集合に現れず、
/// エイリアス名としては通る。**禁止文字を増やす形は採らない。** 集合は AviUtl2
/// の UI が課すものであり、我々が広げると UI から登録できる名前を我々だけが
/// 拒むことになる。防ぐのはパスの組み立ての側であり、[`AliasDirectory`] が
/// 保証する `\\?\` 前置の下では置換が起きないことを実測している。
///
/// **拡張子を付ける形が単独で効いているかは当てにしない。** 実測では
/// `<dir>\NUL.object` は素の連結でもデバイスへ解決しなかったが、`<dir>\NUL` は
/// 解決した。どちらが効いているかは Windows の版に依る話であり、**置換が起き
/// ない形を我々の側で作っておくことと、拡張子が結果的に助けていることは別で
/// ある。**
fn read_alias(dir: &AliasDirectory, name: &str) -> Result<String, AliasRejection> {
    let path = dir.file(name);
    let bytes = match read_bounded(&path, MAX_ALIAS_BYTES as u64) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Err(AliasRejection::TooLarge),
        Err(_) => return Err(AliasRejection::NotFound),
    };
    let text = String::from_utf8(bytes).map_err(|_| AliasRejection::NotParsable)?;
    validate_alias(&text).map_err(|error| match error {
        TextSyntaxError::TooLongBytes { .. } => AliasRejection::TooLarge,
        _ => AliasRejection::NotParsable,
    })?;
    Ok(text)
}

/// 上限を超えないことを確かめながらファイルを読む。
///
/// 上限は開いた直後の大きさで判定し、読み取り自体にも同じ上限を掛ける。判定と
/// 読み取りの間にファイルが伸びても、上限を超えて読むことはない。上限を超えて
/// いれば `Ok(None)` を返す。
fn read_bounded(path: &Path, limit: u64) -> std::io::Result<Option<Vec<u8>>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > limit {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Ok(None);
    }
    Ok(Some(bytes))
}

/// 表としてパースする。安全に扱えない入力は `None` を返す。
///
/// 深さの判定はパースより先に行う。パースそのものは深さで落ちないが、出来上
/// がった表を解放する時点でスタックが尽きる。**受け取ってしまってからでは
/// 捨てることもできない。**
fn parse_table(text: &str) -> Option<Table> {
    if !section_depth_within_limit(text) {
        return None;
    }
    text.parse().ok()
}

/// 節の入れ子が [`MAX_SECTION_DEPTH`] を超えないことを確かめる。
///
/// 深さを増やすのは節の見出しだけである。値の側の `.` は数えない——設定値に
/// 小数が並ぶだけの正当なファイルを落とすことになる。見出しが行を跨ぐ書き方も
/// パーサが受け付けるため、閉じるまで区切りを数え続ける。
fn section_depth_within_limit(text: &str) -> bool {
    let mut pending: Option<usize> = None;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let separators = match pending.take() {
            Some(counted) => counted + line.matches('.').count(),
            None if line.starts_with('[') => line.matches('.').count(),
            None => continue,
        };
        if line.ends_with(']') {
            if separators >= MAX_SECTION_DEPTH {
                return false;
            }
        } else {
            pending = Some(separators);
        }
    }
    true
}

/// パース結果から要約を導く。
fn summarize(table: &Table) -> AliasStructure {
    AliasStructure {
        object_count: object_count(table),
        effects: collect_effects(table),
    }
}

/// 作られるオブジェクト数を判別する。
///
/// 単一オブジェクト形式はルート直下に `Object` を持ち、複数オブジェクト形式は
/// `0` / `1` / … を順に持つ。どちらとも読めない構造では数を名乗らない。判別の
/// 失敗を一覧からの除外にはしない——除外にすると受け入れ規則に 5 つ目の条件が
/// 生える。
fn object_count(table: &Table) -> Option<u32> {
    if table.get_table("Object").is_some() {
        return Some(1);
    }
    let count = table.iter_subtables_as_array().count();
    (count > 0).then_some(count as u32)
}

/// effect 名を出現順に集める。
///
/// 形式に依存せずサブテーブルを辿るだけであり、どんな構造でも成立する。入れ子に
/// せず平坦な並びとするのは、区切る位置が形式を判別できて初めて決まるためである。
///
/// 辿りは明示的なスタックで行う。節の入れ子は入力の側でいくらでも深くでき、
/// 再帰で辿るとその深さがそのままスタックの消費になる。
fn collect_effects(root: &Table) -> Vec<String> {
    let mut effects = Vec::new();
    let mut stack: Vec<&Table> = vec![root];
    while let Some(table) = stack.pop() {
        if let Some(name) = table.get_value(EFFECT_NAME_KEY) {
            effects.push(name.clone());
        }
        let children: Vec<&Table> = table.subtables().map(|(_, child)| child).collect();
        stack.extend(children.into_iter().rev());
    }
    effects
}

/// UI 状態ファイルから label を引く表。
///
/// 引きは前方向に限る。節の直下を列挙する向きは採らない——UI 状態ファイルの
/// エントリが在ることはエイリアスの実体を保証せず、列挙すると作成できない名前を
/// 一覧に載せうる。
#[derive(Debug, Default)]
pub struct LabelTable(Option<Table>);

impl LabelTable {
    /// 引ける項目を 1 つも持たない表を返す。
    pub fn empty() -> Self {
        Self(None)
    }

    /// データディレクトリの UI 状態ファイルを読む。
    ///
    /// 読めない・大きすぎる・解釈できないいずれの場合も空の表を返す。UI 状態
    /// ファイルはエイリアス専用ではなく、そこでの失敗に一覧の可用性を握らせない。
    pub fn read(data_dir: &Path) -> Self {
        Self(read_history(data_dir))
    }

    /// 名前に対応する label を引く。無ければ `None`。
    ///
    /// 既定を補完しない。既定は AviUtl2 側にあり、推測すると UI の表示と食い違う。
    pub fn label_of(&self, name: &str) -> Option<String> {
        self.0
            .as_ref()?
            .get_table(&format!("{HISTORY_ALIAS_SECTION}.{name}"))?
            .get_value(LABEL_KEY)
            .cloned()
    }
}

/// UI 状態ファイルを読んでパースする。失敗はすべて `None` に畳む。
fn read_history(data_dir: &Path) -> Option<Table> {
    let bytes = read_bounded(&data_dir.join(HISTORY_FILE), MAX_HISTORY_INI_BYTES).ok()??;
    parse_table(&String::from_utf8(bytes).ok()?)
}

/// エイリアスの読み取り口。
///
/// 一覧がファイルを開くのはこの口を通してのみである。差し替えれば、窓の外の
/// ファイルを開いていないことを呼び出し回数として数えられる。
pub trait AliasFiles {
    /// 受け入れ規則を 1 件について適用する。
    fn admit(
        &self,
        alias_dir: &AliasDirectory,
        name: &str,
    ) -> Result<AdmittedAlias, AliasRejection>;

    /// label の表を読む。
    fn label_table(&self, data_dir: &Path) -> LabelTable;
}

/// 実ファイルシステムを読む [`AliasFiles`]。
pub struct DiskAliasFiles;

impl AliasFiles for DiskAliasFiles {
    fn admit(
        &self,
        alias_dir: &AliasDirectory,
        name: &str,
    ) -> Result<AdmittedAlias, AliasRejection> {
        admit_alias(alias_dir, name)
    }

    fn label_table(&self, data_dir: &Path) -> LabelTable {
        LabelTable::read(data_dir)
    }
}

/// 登録済みエイリアスを列挙し、要求ページを切り出して返す。
///
/// 手順の順序に意味がある。
///
/// 1. `Alias\*.object` を列挙し、名前の規則で絞る
/// 2. UI 状態ファイルを 1 度だけ読み、ラベルの表を作る
/// 3. label が指定されていれば、ラベルで絞る
/// 4. 名前の昇順に並べる
/// 5. ページを切り出す
/// 6. 切り出した分についてだけファイルを読み、受け入れ規則を通す
///
/// **ファイルを開くのは 6 だけである。** 名前の規則もラベルの絞り込みも中身を
/// 要さないため、パースの費用が窓の大きさで頭打ちになる。ラベルの表は窓の外に
/// 置く——3 の絞り込みが表を要し、絞り込みの後でなければ窓が決まらない。
/// ディレクトリの正規化も 1 度だけであり、窓の件数に比例させない。
///
/// 6 で落ちたエントリは載せず、その分を総件数から引く。1 件のために一覧全体を
/// 落とさない。
pub fn list_object_aliases(
    data_dir: &Path,
    label: Option<&str>,
    page: &PageWindow,
    snapshot_revision: u64,
    files: &dyn AliasFiles,
) -> ListObjectAliasesResult {
    let labels = files.label_table(data_dir);
    let Some(alias_dir) = AliasDirectory::resolve(data_dir) else {
        // ディレクトリが無ければ列挙するものも無い。空のページを返す。
        let (_, meta) = take_window::<(String, Option<String>)>(&[], page, snapshot_revision);
        return ListObjectAliasesResult {
            items: Vec::new(),
            page: meta,
        };
    };
    let mut entries: Vec<(String, Option<String>)> = enumerate_alias_names(&alias_dir)
        .into_iter()
        .map(|name| {
            let label = labels.label_of(&name);
            (name, label)
        })
        .collect();
    if let Some(label) = label {
        entries.retain(|(_, found)| found.as_deref() == Some(label));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    let (window, meta) = take_window(&entries, page, snapshot_revision);
    let mut items = Vec::with_capacity(window.len());
    let mut dropped = 0usize;
    for (name, label) in window {
        match files.admit(&alias_dir, &name) {
            Ok(admitted) => items.push(ObjectAliasSummary {
                name: admitted.name,
                label,
                object_count: admitted.summary.object_count,
                effects: admitted.summary.effects,
            }),
            Err(rejection) => {
                tracing::debug!("エイリアスを一覧から落としました: {rejection}");
                dropped += 1;
            }
        }
    }

    let page = dropped_from_page(meta, dropped, items.len());
    ListObjectAliasesResult { items, page }
}

/// ディレクトリを列挙し、名前の規則を通るものだけを集める。
///
/// 並びは決めない。列挙の順はファイルシステムが決めるものであり、要求元へ返す
/// 並びは呼び出し側が名前で整列して作る。
///
/// 件数は打ち切らない。名前 1 件は小さく、重いのはファイルを開くことであって、
/// それは窓の分に限られている。打ち切ると総件数が嘘になる。
///
/// ディレクトリは除く。判定は列挙が返した属性で決まり、ファイルを開かない。
/// 除かないと、総件数だけが窓を開くまで多い状態になる。**リンクは除かない** —
/// 開けば実体へ届くものを、名前の段で落とさない。
fn enumerate_alias_names(alias_dir: &AliasDirectory) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(alias_dir.path()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| !kind.is_dir()).unwrap_or(true))
        .filter_map(|entry| alias_name_of(&entry.path()))
        .collect()
}

/// パスからエイリアス名を取り出す。規則を通らなければ `None`。
fn alias_name_of(path: &Path) -> Option<String> {
    if !path.extension()?.eq_ignore_ascii_case(ALIAS_EXTENSION) {
        return None;
    }
    let name = path.file_stem()?.to_str()?;
    validate_object_alias_name(name).ok()?;
    Some(name.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use aviutl2_mcp_core::DEFAULT_PAGE_LIMIT;
    use std::cell::Cell;
    use std::collections::BTreeSet;

    /// 全 variant の代表値。新しい variant を足したらここへも足す。
    pub(crate) fn all_rejections() -> Vec<AliasRejection> {
        let mut rejections: Vec<AliasRejection> = TextSyntaxError::ALL
            .iter()
            .map(|source| AliasRejection::ForbiddenName(*source))
            .collect();
        rejections.extend([
            AliasRejection::NotFound,
            AliasRejection::TooLarge,
            AliasRejection::NotParsable,
            AliasRejection::WithoutEffect,
        ]);
        rejections
    }

    /// 単一オブジェクト形式のエイリアス。
    pub(crate) const SINGLE: &str = "[Object]\r\nframe=0,80\r\n[Object.0]\r\neffect.name=テキスト\r\nテキスト=こんにちは\r\n未知=1\r\n[Object.1]\r\neffect.name=標準描画\r\nX=0.0\r\n";

    /// 複数オブジェクト形式のエイリアス。
    const MULTIPLE: &str = "[0]\r\nlayer=0\r\nframe=0,80\r\n[0.0]\r\neffect.name=テキスト\r\nテキスト=ひとつめ\r\n[1]\r\nlayer=1\r\nframe=0,80\r\n[1.0]\r\neffect.name=図形\r\n";

    /// 受け入れ規則の 4 条件を通るが、往復がバイト列を保存しないエイリアス。
    ///
    /// 改行が LF であり、空行を含む。パースして書き戻すと改行は CRLF になり、
    /// 空行は消える。**保存される入力だけを置くと、書き戻す実装でも差が出ず、
    /// 生バイト列を渡していることを確かめられない。**
    pub(crate) const LOSSY: &str = "[Object]\nframe=0,80\n\n[Object.0]\neffect.name=図形\n";

    /// パースはできるが effect 名を 1 つも持たないエイリアス。
    const WITHOUT_EFFECT: &str = "[Object]\r\nframe=0,80\r\n[Object.0]\r\nX=0.0\r\n";

    /// UI 状態ファイル。エイリアス以外の節も持つ。
    const HISTORY: &str = "[Window]\r\nmain=0,0,1280,720\r\n[Effect.object.正常]\r\nlabel=テロップ集\r\nhide=0\r\norder=160\r\n[Effect.object.複数]\r\nlabel=カスタムオブジェクト\r\n";

    /// 一時ディレクトリを作り、後始末を引き受ける番人。
    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn new() -> Self {
            let dir = std::env::temp_dir()
                .join(format!("aviutl2-mcp-alias-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(dir.join(ALIAS_DIRECTORY)).unwrap();
            Self(dir)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }

        /// 正規化を経ていない素のエイリアスディレクトリ。
        fn raw_alias_dir(&self) -> PathBuf {
            self.0.join(ALIAS_DIRECTORY)
        }

        /// 生産経路が使うのと同じ、正規化済みのエイリアスディレクトリ。
        fn alias_dir(&self) -> AliasDirectory {
            AliasDirectory::resolve(&self.0).unwrap()
        }

        pub(crate) fn write_alias(&self, name: &str, contents: &[u8]) {
            std::fs::write(
                self.raw_alias_dir()
                    .join(format!("{name}.{ALIAS_EXTENSION}")),
                contents,
            )
            .unwrap();
        }

        pub(crate) fn write_history(&self, contents: &[u8]) {
            std::fs::write(self.0.join(HISTORY_FILE), contents).unwrap();
        }

        /// 置いたエイリアスをディスク上のバイト列のまま読み直す。
        ///
        /// 受け入れ規則を通さずに読む。規則が返す `raw` と突き合わせる相手は、
        /// 規則の側が組み立てた値であってはならない。
        pub(crate) fn alias_text(&self, name: &str) -> String {
            let bytes = std::fs::read(
                self.raw_alias_dir()
                    .join(format!("{name}.{ALIAS_EXTENSION}")),
            )
            .unwrap();
            String::from_utf8(bytes).unwrap()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// [`super::AliasFiles`] の呼び出し回数を数える口。
    ///
    /// 数えるのは実際にファイルを開いた回数である。成否だけを見るテストでは、
    /// 列挙とページ切り出しの 2 段分割が崩れたことを捕まえられない。
    struct CountingFiles {
        inner: DiskAliasFiles,
        opens: Cell<usize>,
    }

    impl CountingFiles {
        fn new() -> Self {
            Self {
                inner: DiskAliasFiles,
                opens: Cell::new(0),
            }
        }

        fn opens(&self) -> usize {
            self.opens.get()
        }
    }

    impl AliasFiles for CountingFiles {
        fn admit(
            &self,
            alias_dir: &AliasDirectory,
            name: &str,
        ) -> Result<AdmittedAlias, AliasRejection> {
            self.opens.set(self.opens.get() + 1);
            self.inner.admit(alias_dir, name)
        }

        fn label_table(&self, data_dir: &Path) -> LabelTable {
            self.opens.set(self.opens.get() + 1);
            self.inner.label_table(data_dir)
        }
    }

    /// 落ちる条件を 1 つずつ持つ fixture を作る。
    ///
    /// 戻り値はファイル名（拡張子を除いたもの）の一覧である。
    pub(crate) fn write_fixture(dir: &TempDir) -> Vec<String> {
        dir.write_alias("正常", SINGLE.as_bytes());
        dir.write_alias("複数", MULTIPLE.as_bytes());
        dir.write_alias("改行LF", LOSSY.as_bytes());
        dir.write_alias("不正な.名前", SINGLE.as_bytes());
        dir.write_alias("巨大", &oversized_alias());
        dir.write_alias("BOM付き", format!("\u{feff}{SINGLE}").as_bytes());
        dir.write_alias("非UTF8", b"[Object]\r\nname=\xff\xfe\r\n");
        dir.write_alias("効果なし", WITHOUT_EFFECT.as_bytes());
        dir.write_history(HISTORY.as_bytes());
        [
            "正常",
            "複数",
            "改行LF",
            "不正な.名前",
            "巨大",
            "BOM付き",
            "非UTF8",
            "効果なし",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    /// 上限を 1 バイト超えるエイリアス。
    fn oversized_alias() -> Vec<u8> {
        let mut bytes = b"[Object]\r\neffect.name=\xe3\x83\x86".to_vec();
        bytes.resize(MAX_ALIAS_BYTES + 1, b'a');
        bytes
    }

    fn page(offset: u32, limit: u32) -> PageWindow {
        crate::test_support::page_request(offset, limit, None).window()
    }

    fn list(dir: &TempDir, label: Option<&str>, page: &PageWindow) -> ListObjectAliasesResult {
        list_object_aliases(dir.path(), label, page, 7, &DiskAliasFiles)
    }

    fn item_names(result: &ListObjectAliasesResult) -> Vec<String> {
        result.items.iter().map(|item| item.name.clone()).collect()
    }

    #[test]
    fn a_panicking_resolver_yields_no_directory() {
        // 設定ハンドルの初期化状態を問い合わせる手段が無いため、解決は panic で
        // しか失敗を知らせない。捕捉して型のある不在へ落とす。
        assert_eq!(
            resolve_data_directory(|| panic!("Config handle not initialized")),
            None
        );
        assert_eq!(
            resolve_data_directory(|| PathBuf::from(r"C:\aviutl2\data")),
            Some(PathBuf::from(r"C:\aviutl2\data"))
        );
    }

    #[test]
    fn every_rejection_carries_its_own_code_and_reason() {
        // 受け入れ規則の 4 条件は要求元が次に取る行動がそれぞれ違う。1 つでも
        // 同じ応答になると切り分けられない。
        let mapped: Vec<(ErrorCode, Value)> = [
            AliasRejection::ForbiddenName(TextSyntaxError::Empty),
            AliasRejection::ForbiddenName(TextSyntaxError::ForbiddenCharacter),
            AliasRejection::ForbiddenName(TextSyntaxError::ContainsNul),
            AliasRejection::NotFound,
            AliasRejection::TooLarge,
            AliasRejection::NotParsable,
            AliasRejection::WithoutEffect,
        ]
        .into_iter()
        .map(|rejection| (rejection.error_code(), rejection.details()))
        .collect();

        assert_eq!(
            mapped,
            vec![
                (ErrorCode::InvalidArgument, json!({ "reason": "empty" })),
                (
                    ErrorCode::InvalidArgument,
                    json!({ "reason": "forbidden_character" })
                ),
                (
                    ErrorCode::InvalidArgument,
                    json!({ "reason": "contains_nul" })
                ),
                (ErrorCode::NotFound, json!({})),
                (ErrorCode::InvalidArgument, json!({ "reason": "too_long" })),
                (
                    ErrorCode::InvalidArgument,
                    json!({ "reason": REASON_ALIAS_NOT_PARSABLE })
                ),
                (
                    ErrorCode::InvalidArgument,
                    json!({ "reason": REASON_ALIAS_WITHOUT_EFFECT })
                ),
            ]
        );
    }

    #[test]
    fn a_rejection_never_echoes_the_name_or_the_contents() {
        for rejection in all_rejections() {
            let text = format!("{} {}", rejection, rejection.details());
            for forbidden in ["こんにちは", "テキスト", "\\", "/"] {
                assert!(!text.contains(forbidden), "{text}");
            }
        }
    }

    #[test]
    fn the_admission_rule_falls_at_the_first_condition_that_does_not_hold() {
        let dir = TempDir::new();
        write_fixture(&dir);
        let alias_dir = dir.alias_dir();

        assert!(admit_alias(&alias_dir, "正常").is_ok());
        assert!(admit_alias(&alias_dir, "複数").is_ok());
        assert_eq!(
            admit_alias(&alias_dir, "不正な.名前"),
            Err(AliasRejection::ForbiddenName(
                TextSyntaxError::ForbiddenCharacter
            ))
        );
        assert_eq!(
            admit_alias(&alias_dir, "存在しない"),
            Err(AliasRejection::NotFound)
        );
        assert_eq!(
            admit_alias(&alias_dir, "巨大"),
            Err(AliasRejection::TooLarge)
        );
        assert_eq!(
            admit_alias(&alias_dir, "BOM付き"),
            Err(AliasRejection::NotParsable)
        );
        assert_eq!(
            admit_alias(&alias_dir, "非UTF8"),
            Err(AliasRejection::NotParsable)
        );
        assert_eq!(
            admit_alias(&alias_dir, "効果なし"),
            Err(AliasRejection::WithoutEffect)
        );
    }

    #[test]
    fn a_deeply_nested_section_is_refused_before_it_is_parsed() {
        // 表は入れ子を再帰的に解放する。深い入れ子は 20 KB 程度の入力でも
        // スタックを使い切り、捕捉層では受け止められないままプロセスごと落ちる。
        let deep = format!(
            "[{}]\r\nk=v\r\n",
            vec!["a"; MAX_SECTION_DEPTH + 1].join(".")
        );
        assert!(!section_depth_within_limit(&deep));
        assert_eq!(parse_table(&deep), None);

        // 行を跨いで綴っても同じ深さである。閉じるまで数え続けなければ、
        // 分割するだけで判定を抜けられる。
        let split = format!(
            "[{}\r\n{}]\r\nk=v\r\n",
            vec!["a"; MAX_SECTION_DEPTH].join("."),
            vec!["a"; MAX_SECTION_DEPTH].join(".")
        );
        assert!(!section_depth_within_limit(&split));

        // 値の側の区切りは数えない。小数が並ぶだけの正当なファイルは通る。
        let values = format!("[Object.0]\r\n{}\r\n", "X=0.0\r\n".repeat(1_000));
        assert!(section_depth_within_limit(&values));
        assert!(section_depth_within_limit(SINGLE));
        assert!(section_depth_within_limit(MULTIPLE));
        assert!(section_depth_within_limit(HISTORY));
    }

    /// パースした表の入れ子の深さを測る。
    ///
    /// 判定はパーサのトークン化をテキストの上で写したものである。写しである
    /// 以上、上流が変われば黙って乖離する。実際にパースして測った深さと突き
    /// 合わせることで、写しの前提そのものを固定する。
    fn measured_depth(table: &Table) -> usize {
        let mut deepest = 0;
        let mut stack: Vec<(&Table, usize)> = vec![(table, 0)];
        while let Some((current, depth)) = stack.pop() {
            deepest = deepest.max(depth);
            stack.extend(current.subtables().map(|(_, child)| (child, depth + 1)));
        }
        deepest
    }

    #[test]
    fn what_the_depth_check_admits_really_parses_within_the_limit() {
        // 判定が通した入力は、実際にパースしても上限を超えない。超える入力を
        // 通してしまえば、表を捨てる時点でスタックが尽きる。
        let deep = vec!["a"; MAX_SECTION_DEPTH - 1].join(".");
        let adversarial = [
            SINGLE.to_string(),
            MULTIPLE.to_string(),
            HISTORY.to_string(),
            // 上限ちょうど。
            format!("[{deep}]\r\nk=v\r\n"),
            // 見出しが行を跨ぐ。パーサは閉じるまで名前として読み続ける。
            "[a.b\r\nc.d]\r\nk=v\r\n".to_string(),
            // 値の側に区切りが並ぶ。深さは増えない。
            "[Object.0]\r\nX=0.0\r\nY=1.2.3\r\n".to_string(),
            // 見出しの後に別の浅い見出しが来る。深さは最大値で決まる。
            format!("[{deep}]\r\nk=v\r\n[Object]\r\nk=v\r\n"),
            // 空の見出し。
            "[]\r\nk=v\r\n".to_string(),
            // 節を持たない。
            "k=v\r\n".to_string(),
        ];

        for source in adversarial {
            assert!(
                section_depth_within_limit(&source),
                "判定が落としました: {source:?}"
            );
            let table = parse_table(&source).unwrap_or_else(|| panic!("{source:?}"));
            assert!(
                measured_depth(&table) <= MAX_SECTION_DEPTH,
                "測った深さが上限を超えました: {source:?}"
            );
        }
    }

    #[test]
    fn a_deeply_nested_alias_is_dropped_from_the_listing() {
        let dir = TempDir::new();
        dir.write_alias("正常", SINGLE.as_bytes());
        dir.write_alias(
            "深い",
            format!(
                "[{}]\r\neffect.name=図形\r\n",
                vec!["a"; MAX_SECTION_DEPTH + 1].join(".")
            )
            .as_bytes(),
        );

        assert_eq!(
            admit_alias(&dir.alias_dir(), "深い"),
            Err(AliasRejection::NotParsable)
        );
        let result = list(&dir, None, &page(0, DEFAULT_PAGE_LIMIT));
        assert_eq!(item_names(&result), vec!["正常"]);
    }

    #[test]
    fn a_deeply_nested_history_leaves_every_label_empty() {
        // 深い節だけを置くと、深さの判定が無くても label は引けない——その
        // ファイルは Effect.object.<名前> を持たないためである。判定が効いて
        // いることは、引ける label を同じファイルへ同居させて初めて見える。
        let dir = TempDir::new();
        dir.write_alias("正常", SINGLE.as_bytes());
        dir.write_history(
            format!(
                "{HISTORY}[{}]\r\nlabel=x\r\n",
                vec!["a"; MAX_SECTION_DEPTH + 1].join(".")
            )
            .as_bytes(),
        );

        // 同じ内容から深い節だけを除けば label は引ける。差は深さだけである。
        let shallow = TempDir::new();
        shallow.write_alias("正常", SINGLE.as_bytes());
        shallow.write_history(HISTORY.as_bytes());
        assert_eq!(
            list(&shallow, None, &page(0, DEFAULT_PAGE_LIMIT)).items[0].label,
            Some("テロップ集".to_string())
        );

        let result = list(&dir, None, &page(0, DEFAULT_PAGE_LIMIT));
        assert_eq!(item_names(&result), vec!["正常"]);
        assert!(result.items.iter().all(|item| item.label.is_none()));
    }

    #[test]
    fn a_reserved_device_name_is_not_opened_as_a_device() {
        // 予約デバイス名は禁止文字の 14 種では落ちない。開く経路がデバイスへ
        // 解決すると、読み取りが戻らないことがある。
        let dir = TempDir::new();
        let alias_dir = dir.alias_dir();

        // 素の連結ではデバイスへ解決する。ここで固定するのは Windows の挙動
        // そのものであり、我々が防いでいる対象である。
        assert!(
            File::open(dir.raw_alias_dir().join("NUL")).is_ok(),
            "素の連結でデバイスへ解決しませんでした"
        );
        // 受け入れ規則が使うディレクトリは置換の起きない形になっている。
        assert!(
            File::open(alias_dir.path().join("NUL")).is_err(),
            "正規化したディレクトリの下でデバイスへ解決しました"
        );

        for name in ["CON", "NUL", "PRN", "AUX", "COM1"] {
            assert_eq!(
                admit_alias(&alias_dir, name),
                Err(AliasRejection::NotFound),
                "{name}"
            );
        }
    }

    #[test]
    fn the_summary_counts_the_objects_of_each_layout() {
        let dir = TempDir::new();
        write_fixture(&dir);
        let alias_dir = dir.alias_dir();

        let single = admit_alias(&alias_dir, "正常").unwrap();
        assert_eq!(single.summary.object_count, Some(1));
        assert_eq!(single.summary.effects, vec!["テキスト", "標準描画"]);

        let multiple = admit_alias(&alias_dir, "複数").unwrap();
        assert_eq!(multiple.summary.object_count, Some(2));
        assert_eq!(multiple.summary.effects, vec!["テキスト", "図形"]);
    }

    #[test]
    fn an_unrecognisable_layout_does_not_claim_an_object_count() {
        // 形式を判別できないことを一覧からの除外にはしない。除外にすると
        // 受け入れ規則に 5 つ目の条件が生える。
        let dir = TempDir::new();
        dir.write_alias("手置き", "[なにか]\r\neffect.name=図形\r\n".as_bytes());

        let admitted = admit_alias(&dir.alias_dir(), "手置き").unwrap();
        assert_eq!(admitted.summary.object_count, None);
        assert_eq!(admitted.summary.effects, vec!["図形"]);
    }

    #[test]
    fn the_effects_keep_their_order_and_their_duplicates() {
        let dir = TempDir::new();
        dir.write_alias(
            "重複",
            "[0]\r\n[0.0]\r\neffect.name=図形\r\n[0.1]\r\neffect.name=図形\r\n[1]\r\n[1.0]\r\neffect.name=テキスト\r\n".as_bytes(),
        );

        let admitted = admit_alias(&dir.alias_dir(), "重複").unwrap();
        assert_eq!(admitted.summary.effects, vec!["図形", "図形", "テキスト"]);
    }

    #[test]
    fn the_parser_still_round_trips_the_files_the_host_saves() {
        // 生産経路は往復に依存していないが、上流の版を上げたときに気付ける
        // 唯一の場所である。併せて、生産経路が実際に依存している 3 点——パース
        // が成功すること・未知のキーで失敗しないこと・effect 名を取り出せること
        // ——を同じ入力で固定する。
        for source in [SINGLE, MULTIPLE] {
            let table: Table = source.parse().unwrap();
            assert_eq!(table.to_string(), source);
            assert!(!collect_effects(&table).is_empty());
        }

        let single: Table = SINGLE.parse().unwrap();
        assert_eq!(
            single.get_table("Object.0").unwrap().get_value("未知"),
            Some(&"1".to_string())
        );
    }

    #[test]
    fn a_label_is_looked_up_forwards_by_name() {
        let dir = TempDir::new();
        write_fixture(&dir);
        let labels = LabelTable::read(dir.path());

        assert_eq!(labels.label_of("正常"), Some("テロップ集".to_string()));
        assert_eq!(
            labels.label_of("複数"),
            Some("カスタムオブジェクト".to_string())
        );
        assert_eq!(labels.label_of("効果なし"), None);
        assert_eq!(LabelTable::empty().label_of("正常"), None);
    }

    #[test]
    fn items_and_the_admission_rule_agree_for_every_name_in_the_window() {
        // 窓に入った 1 件ずつについての言明である。集合の等式は fixture が
        // 1 ページに収まるときしか成り立たない。
        let dir = TempDir::new();
        write_fixture(&dir);
        let mut names = enumerate_alias_names(&dir.alias_dir());
        names.sort();
        assert!(names.len() > 2);

        for offset in 0..names.len() {
            let result = list(&dir, None, &page(offset as u32, 2));
            let listed: BTreeSet<String> = item_names(&result).into_iter().collect();
            for name in &names[offset..(offset + 2).min(names.len())] {
                assert_eq!(
                    listed.contains(name),
                    admit_alias(&dir.alias_dir(), name).is_ok(),
                    "offset {offset} の {name}"
                );
            }
        }
    }

    #[test]
    fn every_name_missing_from_the_items_is_rejected_with_the_documented_failure() {
        let dir = TempDir::new();
        let fixture = write_fixture(&dir);
        let result = list(&dir, None, &page(0, DEFAULT_PAGE_LIMIT));
        let listed: BTreeSet<String> = item_names(&result).into_iter().collect();

        let expected = [
            (
                "不正な.名前",
                ErrorCode::InvalidArgument,
                Some("forbidden_character"),
            ),
            ("巨大", ErrorCode::InvalidArgument, Some("too_long")),
            (
                "BOM付き",
                ErrorCode::InvalidArgument,
                Some(REASON_ALIAS_NOT_PARSABLE),
            ),
            (
                "非UTF8",
                ErrorCode::InvalidArgument,
                Some(REASON_ALIAS_NOT_PARSABLE),
            ),
            (
                "効果なし",
                ErrorCode::InvalidArgument,
                Some(REASON_ALIAS_WITHOUT_EFFECT),
            ),
        ];

        for name in &fixture {
            if listed.contains(name) {
                continue;
            }
            let (_, code, reason) = expected
                .iter()
                .find(|(candidate, _, _)| candidate == name)
                .unwrap_or_else(|| panic!("{name} の失敗が表にありません"));
            let rejection = admit_alias(&dir.alias_dir(), name).unwrap_err();
            assert_eq!(rejection.error_code(), *code, "{name}");
            assert_eq!(rejection.reason(), *reason, "{name}");
        }
    }

    #[test]
    fn every_name_in_the_items_carries_the_bytes_that_were_read() {
        // 作成経路が SDK へ渡すのは読み取った生バイト列そのものである。
        let dir = TempDir::new();
        write_fixture(&dir);
        let result = list(&dir, None, &page(0, DEFAULT_PAGE_LIMIT));
        assert!(!result.items.is_empty());

        for item in &result.items {
            let admitted = admit_alias(&dir.alias_dir(), &item.name).unwrap();
            let on_disk = std::fs::read(
                dir.raw_alias_dir()
                    .join(format!("{}.{ALIAS_EXTENSION}", item.name)),
            )
            .unwrap();
            assert_eq!(admitted.raw.as_bytes(), on_disk.as_slice(), "{}", item.name);
        }
    }

    #[test]
    fn paging_through_the_whole_directory_yields_exactly_the_admitted_names() {
        // 窓を跨いだ和集合なら集合の等式が成り立つ。既定の limit では fixture が
        // 1 ページに収まり、ページングが壊れていても通ってしまう。
        let dir = TempDir::new();
        let fixture = write_fixture(&dir);

        let mut collected: Vec<String> = Vec::new();
        let mut offset = 0;
        loop {
            let result = list(&dir, None, &page(offset, 2));
            collected.extend(item_names(&result));
            match result.page.next_offset {
                Some(next) => offset = next,
                None => break,
            }
        }

        let unique: BTreeSet<String> = collected.iter().cloned().collect();
        assert_eq!(unique.len(), collected.len(), "ページを跨いで重複しました");

        let admitted: BTreeSet<String> = fixture
            .iter()
            .filter(|name| admit_alias(&dir.alias_dir(), name).is_ok())
            .cloned()
            .collect();
        assert_eq!(unique, admitted);
    }

    #[test]
    fn the_listing_reports_the_revision_it_was_given() {
        // 照合を外すのは要求を組み立てる側であり、切り出しは受け取った指定に
        // そのまま従う。ページのメタ情報が表す意味は列挙時点の revision である。
        let dir = TempDir::new();
        write_fixture(&dir);

        let result = list(&dir, None, &page(0, 2));
        assert_eq!(result.page.snapshot_revision, 7);
    }

    #[test]
    fn the_listing_opens_only_the_files_in_the_window() {
        // 名前の規則を通るファイルを limit より多く置く。開いた回数が窓の分と
        // UI 状態ファイルの 1 回に収まらなければ、切り出しより前に読んでいる。
        let dir = TempDir::new();
        for index in 0..8 {
            dir.write_alias(&format!("項目{index}"), SINGLE.as_bytes());
        }
        dir.write_history(HISTORY.as_bytes());

        let files = CountingFiles::new();
        let limit = 2;
        let result = list_object_aliases(dir.path(), None, &page(0, limit), 7, &files);

        assert_eq!(result.items.len(), limit as usize);
        assert_eq!(result.page.total_count, 8);
        assert!(
            files.opens() <= limit as usize + 1,
            "{} 回開きました",
            files.opens()
        );
    }

    #[test]
    fn a_single_rejected_entry_inside_the_window_leaves_the_rest_of_the_page() {
        let dir = TempDir::new();
        dir.write_alias("あ", SINGLE.as_bytes());
        dir.write_alias("い", WITHOUT_EFFECT.as_bytes());
        dir.write_alias("う", SINGLE.as_bytes());

        let result = list(&dir, None, &page(0, DEFAULT_PAGE_LIMIT));
        assert_eq!(item_names(&result), vec!["あ", "う"]);
        assert_eq!(result.page.total_count, 2);
        assert_eq!(result.page.count, 2);
    }

    #[test]
    fn a_rejected_entry_outside_the_window_is_not_subtracted_from_the_total() {
        // 総件数から引かれるのは本ページで落とした分だけである。まだ開いていない
        // ページに何件落ちるものがあるかは、読まずには数えられない。
        let dir = TempDir::new();
        dir.write_alias("あ", SINGLE.as_bytes());
        dir.write_alias("い", SINGLE.as_bytes());
        dir.write_alias("う", WITHOUT_EFFECT.as_bytes());

        let result = list(&dir, None, &page(0, 2));
        assert_eq!(item_names(&result), vec!["あ", "い"]);
        assert_eq!(result.page.total_count, 3);

        let second = list(&dir, None, &page(2, 2));
        assert!(second.items.is_empty());
        assert_eq!(second.page.total_count, 2);
    }

    #[test]
    fn the_label_filter_applies_before_the_window_is_cut() {
        // 絞り込みを切り出しの後に置くと、窓の中に残った分しか返らない。
        let dir = TempDir::new();
        for index in 0..4 {
            dir.write_alias(&format!("その他{index}"), SINGLE.as_bytes());
        }
        dir.write_alias("目当て", SINGLE.as_bytes());
        dir.write_history("[Effect.object.目当て]\r\nlabel=テロップ集\r\n".as_bytes());

        let result = list(&dir, Some("テロップ集"), &page(0, 2));
        assert_eq!(item_names(&result), vec!["目当て"]);
        assert_eq!(result.page.total_count, 1);
    }

    /// 列挙が返す生の並びと、名前の昇順が食い違う名前。
    ///
    /// ディレクトリの列挙は大文字化した UTF-16 の照合順で返り、`String` の
    /// 比較は符号位置の順である。**大小が混ざらない名前だけを並べると 2 つの
    /// 順が一致してしまい、整列そのものを消しても気付けない。**
    const COLLATION_SENSITIVE: [&str; 5] = ["a", "B", "z", "あ", "お"];

    #[test]
    fn the_items_are_sorted_by_name_across_pages_regardless_of_creation_order() {
        let dir = TempDir::new();
        for name in COLLATION_SENSITIVE {
            dir.write_alias(name, SINGLE.as_bytes());
        }

        // 列挙が返す生の並びが、期待する並びと実際に違うことを先に確かめる。
        // 一致してしまう fixture では、この検査は何も守っていない。
        let enumerated = enumerate_alias_names(&dir.alias_dir());
        let mut expected = enumerated.clone();
        expected.sort();
        assert_ne!(
            enumerated, expected,
            "列挙の順と名前の昇順が一致する fixture では整列を確かめられません"
        );

        let mut collected = Vec::new();
        let mut offset = 0;
        loop {
            let result = list(&dir, None, &page(offset, 2));
            collected.extend(item_names(&result));
            match result.page.next_offset {
                Some(next) => offset = next,
                None => break,
            }
        }
        assert_eq!(collected, expected);

        let reversed = TempDir::new();
        for name in COLLATION_SENSITIVE.iter().rev() {
            reversed.write_alias(name, SINGLE.as_bytes());
        }
        let result = list(&reversed, None, &page(0, DEFAULT_PAGE_LIMIT));
        assert_eq!(item_names(&result), collected);
    }

    #[test]
    fn a_directory_that_looks_like_an_alias_is_not_counted() {
        // 判定は列挙が返した属性で決まり、ファイルを開かない。除かないと、
        // 総件数だけが窓を開くまで多い状態になる。
        let dir = TempDir::new();
        dir.write_alias("正常", SINGLE.as_bytes());
        std::fs::create_dir_all(dir.raw_alias_dir().join("紛らわしい.object")).unwrap();

        let result = list(&dir, None, &page(0, DEFAULT_PAGE_LIMIT));
        assert_eq!(item_names(&result), vec!["正常"]);
        assert_eq!(result.page.total_count, 1);
    }

    #[test]
    fn a_missing_alias_directory_yields_an_empty_listing() {
        let dir = TempDir::new();
        std::fs::remove_dir_all(dir.raw_alias_dir()).unwrap();

        let result = list(&dir, None, &page(0, DEFAULT_PAGE_LIMIT));
        assert!(result.items.is_empty());
        assert_eq!(result.page.total_count, 0);
    }

    #[test]
    fn a_history_file_that_cannot_be_used_leaves_every_label_empty() {
        for history in [
            None,
            Some(b"[Effect.object".to_vec()),
            Some(oversized_history()),
        ] {
            let dir = TempDir::new();
            dir.write_alias("正常", SINGLE.as_bytes());
            if let Some(history) = history {
                dir.write_history(&history);
            }

            let result = list(&dir, None, &page(0, DEFAULT_PAGE_LIMIT));
            assert_eq!(item_names(&result), vec!["正常"]);
            assert!(result.items.iter().all(|item| item.label.is_none()));
        }
    }

    /// 上限を 1 バイト超える UI 状態ファイル。
    fn oversized_history() -> Vec<u8> {
        let mut bytes = b"[Effect.object.\xe6\xad\xa3\xe5\xb8\xb8]\r\nlabel=x\r\n".to_vec();
        bytes.resize(MAX_HISTORY_INI_BYTES as usize + 1, b'\n');
        bytes
    }

    #[test]
    fn the_listing_never_carries_the_alias_text() {
        let dir = TempDir::new();
        write_fixture(&dir);

        let result = list(&dir, None, &page(0, DEFAULT_PAGE_LIMIT));
        let document = serde_json::to_string(&result).unwrap();
        for forbidden in ["こんにちは", "frame=0,80", "[Object]", "ひとつめ"] {
            assert!(
                !document.contains(forbidden),
                "{forbidden} が応答に含まれます: {document}"
            );
        }
    }

    #[test]
    fn the_listing_reports_the_label_it_found() {
        let dir = TempDir::new();
        write_fixture(&dir);

        let result = list(&dir, None, &page(0, DEFAULT_PAGE_LIMIT));
        let labels: Vec<(String, Option<String>)> = result
            .items
            .iter()
            .map(|item| (item.name.clone(), item.label.clone()))
            .collect();
        assert_eq!(
            labels,
            vec![
                // UI 状態ファイルに項目を持たない名前は label を欠く。既定を
                // 補完しない。
                ("改行LF".to_string(), None),
                ("正常".to_string(), Some("テロップ集".to_string())),
                ("複数".to_string(), Some("カスタムオブジェクト".to_string())),
            ]
        );
    }
}
