//! 保護 DACL をテストから検証するための補助。
//!
//! 検証そのものは [`crate::create_protected_directory`] の内部にあり、製品
//! コードからは観測できない。呼び出し側の crate が「自分が用意させた対象が
//! 実際に保護されている」ことを確かめられるよう、`test-support` feature の
//! 下でだけ表明の形で公開する。

use crate::dacl::SUBJECT_COUNT;
use crate::verify::{allowed_subjects, read_security_descriptor, verify_protected_dacl};
use std::path::Path;

/// 対象の DACL が 3 主体だけを許可し、継承が無効であることを表明する。
///
/// 満たさない場合は panic する。表明の説明に対象パスを含めない。
pub fn assert_protected_dacl(path: &Path) {
    verify_protected_dacl(path).expect("DACL が想定と異なります");
    let descriptor = read_security_descriptor(path).expect("DACL を読み出せません");
    let seen = allowed_subjects(&descriptor).expect("DACL が想定と異なります");
    assert_eq!(
        seen, [true; SUBJECT_COUNT],
        "許可する ACE を持たない主体があります"
    );
}

/// 対象のセキュリティ記述子をバイト列として読み出す。
///
/// 呼び出しの前後で読み比べると、DACL が書き換えられていないことを確かめ
/// られる。**成否だけを見ても「壊さないこと」は確かめられない。**
pub fn security_descriptor_bytes(path: &Path) -> Vec<u8> {
    read_security_descriptor(path)
        .expect("DACL を読み出せません")
        .bytes()
        .to_vec()
}
