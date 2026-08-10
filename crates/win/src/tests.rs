//! 実ファイルシステムに対する DACL の作成と検証の確認。
//!
//! DACL の構築も検証も Win32 API の振る舞いそのものであり、模擬に置き換えると
//! 確かめたい性質が消える。`%TEMP%` の下に一意な名前で作り、必ず片付ける。

use crate::dacl::{ProtectedSecurityAttributes, protected_sids, well_known_sid};
use crate::verify::{allowed_subjects, dacl_is_protected, read_security_descriptor};
use crate::wide::to_wide;
use crate::{ProtectedDirError, create_protected_directory, create_protected_file};
use std::io::Write;
use std::path::{Path, PathBuf};
use windows::Win32::Security::WinWorldSid;
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::core::PCWSTR;

/// 破棄時に消える一時ディレクトリの名前。
///
/// 名前を作るだけで、実体は各テストが目的の方式で作る。表明が失敗しても
/// 片付けが飛ばないよう、削除は破棄に任せる。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("aviutl2-mcp-win-test-{}", uuid::Uuid::new_v4())))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 対象のセキュリティ記述子をバイト列として読み出す。
fn dacl_bytes(path: &Path) -> Vec<u8> {
    read_security_descriptor(path)
        .expect("DACL を読み出せません")
        .bytes()
        .to_vec()
}

/// 3 主体に加えて Everyone を許可するディレクトリを作る。
///
/// 継承は無効化されているため、失敗の理由は「主体が広い」ことだけになる。
fn create_directory_allowing_everyone(path: &Path) {
    let mut sids = protected_sids().expect("3 主体の SID を作れません");
    // SAFETY: 生成した SID は `sids` が保持する。
    sids.push(unsafe { well_known_sid(WinWorldSid) }.expect("Everyone の SID を作れません"));
    let attributes = ProtectedSecurityAttributes::for_sids(sids)
        .expect("Everyone を含むセキュリティ属性を組み立てられません");
    let wide = to_wide(path);
    // SAFETY: `wide` は NUL 終端したパスであり、`attributes` は本呼び出しの間
    // 生存する。
    unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(attributes.as_ptr())) }
        .expect("Everyone を許可するディレクトリを作れません");
}

#[test]
fn a_new_directory_allows_only_the_three_subjects() {
    let dir = TempDir::new();
    create_protected_directory(dir.path()).expect("保護されたディレクトリを作成できません");

    let descriptor = read_security_descriptor(dir.path()).expect("DACL を読み出せません");
    assert!(
        dacl_is_protected(&descriptor).expect("継承の設定を読み出せません"),
        "新規作成したディレクトリの継承が無効化されていません"
    );
    let seen =
        allowed_subjects(&descriptor).expect("新規作成したディレクトリの DACL が想定と異なります");
    assert_eq!(seen, [true; 3], "許可する ACE を持たない主体があります");
}

#[test]
fn a_file_created_inside_inherits_the_restriction() {
    // 成果物のファイルへ個別に DACL を設定していないため、継承した ACE が
    // ファイル単位の保護の根拠になる。
    let dir = TempDir::new();
    create_protected_directory(dir.path()).expect("保護されたディレクトリを作成できません");
    let file = dir.path().join("artifact.png");
    std::fs::write(&file, b"payload").expect("ファイルを作成できません");

    // 継承の無効化は設定した対象自身の属性であり、そこから継承を受けた
    // ファイルには付かない。ファイルについて言えるのは、許可される主体が
    // 親から継承した 3 主体だけであることである。
    let descriptor = read_security_descriptor(&file).expect("DACL を読み出せません");
    let seen = allowed_subjects(&descriptor).expect("継承したファイルの DACL が想定と異なります");
    assert_eq!(seen, [true; 3], "許可する ACE を持たない主体があります");
}

#[test]
fn an_existing_directory_is_verified_and_left_untouched() {
    // 「失敗すること」と「壊さないこと」を同じテストで確かめる。前後の
    // セキュリティ記述子をバイト列で読み比べるため、成否だけが合っていて
    // 中身を書き換える実装はここで落ちる。

    // 1. 保護済みの既存ディレクトリ。成功し、DACL は 1 ビットも変わらない。
    let protected = TempDir::new();
    create_protected_directory(protected.path()).expect("保護されたディレクトリを作成できません");
    let before = dacl_bytes(protected.path());
    create_protected_directory(protected.path())
        .expect("保護済みの既存ディレクトリが受け入れられません");
    assert_eq!(
        dacl_bytes(protected.path()),
        before,
        "保護済みの既存ディレクトリの DACL が書き換わりました"
    );

    // 2. 継承が有効な既存ディレクトリ。失敗し、DACL は変わらない。
    let inherited = TempDir::new();
    std::fs::create_dir_all(inherited.path()).expect("ディレクトリを作成できません");
    let before = dacl_bytes(inherited.path());
    let error = create_protected_directory(inherited.path())
        .expect_err("継承が有効な既存ディレクトリが受け入れられました");
    assert!(
        matches!(error, ProtectedDirError::NotProtected),
        "{error:?}"
    );
    assert_eq!(
        dacl_bytes(inherited.path()),
        before,
        "継承が有効な既存ディレクトリの DACL が書き換わりました"
    );

    // 3. 3 主体以外を許可する既存ディレクトリ。失敗し、DACL は変わらない。
    let widened = TempDir::new();
    create_directory_allowing_everyone(widened.path());
    let before = dacl_bytes(widened.path());
    let error = create_protected_directory(widened.path())
        .expect_err("3 主体以外を許可する既存ディレクトリが受け入れられました");
    assert!(
        matches!(error, ProtectedDirError::NotProtected),
        "{error:?}"
    );
    assert_eq!(
        dacl_bytes(widened.path()),
        before,
        "3 主体以外を許可する既存ディレクトリの DACL が書き換わりました"
    );
}

#[test]
fn a_directory_that_cannot_be_created_is_not_reported_as_unprotected() {
    // 「作れなかった」と「作れたが信用できない」を取り違えない。親が無い場合は
    // 検証へ回らず、そのまま失敗を返す。
    let dir = TempDir::new();
    let nested = dir.path().join("missing").join("child");

    let error = create_protected_directory(&nested).expect_err("親の無いディレクトリを作れました");
    // 理由は種別として残す。権限が無いのか、親が無いのかで呼び出し元の対処が
    // 違うため、まとめて「その他」へ畳まない。
    let ProtectedDirError::Io(error) = error else {
        panic!("作れなかった失敗が入出力の失敗として返りません: {error:?}");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error:?}");
}

#[test]
fn a_protected_file_is_never_created_over_an_existing_one() {
    let dir = TempDir::new();
    create_protected_directory(dir.path()).expect("保護されたディレクトリを作成できません");
    let path = dir.path().join("descriptor.json");

    let mut file = create_protected_file(&path).expect("保護されたファイルを作成できません");
    file.write_all(b"{}").expect("ファイルへ書き込めません");
    drop(file);

    let descriptor = read_security_descriptor(&path).expect("DACL を読み出せません");
    assert!(
        dacl_is_protected(&descriptor).expect("継承の設定を読み出せません"),
        "新規作成したファイルの継承が無効化されていません"
    );
    let seen =
        allowed_subjects(&descriptor).expect("新規作成したファイルの DACL が想定と異なります");
    assert_eq!(seen, [true; 3], "許可する ACE を持たない主体があります");

    let error = create_protected_file(&path).expect_err("既存のファイルを作り直せました");
    let ProtectedDirError::Io(error) = error else {
        panic!("既存のファイルが検証の失敗として返りました: {error:?}");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists, "{error:?}");
}
