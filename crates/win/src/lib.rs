//! Win32 の境界で使うプリミティブ。
//!
//! パスを Win32 へ渡す形へ直す [`to_wide`] と、保護された DACL を持つ
//! ディレクトリ・ファイルの用意を提供する。
//!
//! 成果物にも registry にも利用者のプロジェクトの内容が写る。既定の継承 ACE を
//! そのまま受け取ると、親ディレクトリの権限次第で他のユーザーへ読み取りが開く
//! ため、現在のユーザー SID・SYSTEM・Administrators の 3 主体だけを許可し、
//! 継承を無効化した DACL を明示的に与える。
//!
//! # 既存のオブジェクトは書き換えない
//!
//! 用意する対象が既に存在していた場合、この crate は **DACL を検証するだけ**で
//! あり、設定し直さない。「既に在った」ことは「我々が作った」ことを意味せず、
//! 元の DACL を保存していない以上、上書きは戻せない変更になる。想定と異なる
//! DACL を持つ既存のディレクトリは [`ProtectedDirError::NotProtected`] として
//! 失敗させ、呼び出し元が起動を諦める形にする。
//!
//! 締め直しは保証にもならない。信頼境界は同一の Windows ユーザーであり、その
//! 利用者は自分のディレクトリの ACL をいつでも変えられる。
//!
//! # 失敗の説明に対象パスを含めない
//!
//! 利用者のディレクトリ構成をログへ残さないため、どの対象で失敗したかは
//! 呼び出し元が匿名化した呼び名で添える。

#[cfg(windows)]
mod dacl;
#[cfg(windows)]
mod protected;
#[cfg(all(windows, feature = "test-support"))]
pub mod test_support;
#[cfg(all(windows, test))]
mod tests;
#[cfg(windows)]
mod verify;
#[cfg(windows)]
mod wide;

#[cfg(windows)]
pub use dacl::ProtectedSecurityAttributes;
#[cfg(windows)]
pub use protected::{create_protected_directory, create_protected_file};
#[cfg(windows)]
pub use wide::to_wide;

/// 保護されたディレクトリ・ファイルの用意に失敗した理由。
///
/// 「作れなかった」と「作れたが信用できない」を分ける。運用者が取る行動が
/// 違うためである。前者は権限や空き容量の問題であり、後者は対象を作り直すか
/// 削除する必要がある。
#[derive(Debug, thiserror::Error)]
pub enum ProtectedDirError {
    /// 既に存在するが、DACL が想定と異なる。
    ///
    /// 我々が作ったものではない可能性がある。**書き換えない。**
    #[error("既存のディレクトリの DACL が想定と異なります")]
    NotProtected,
    /// Win32 の失敗。
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<ProtectedDirError> for std::io::Error {
    fn from(error: ProtectedDirError) -> Self {
        match error {
            ProtectedDirError::Io(error) => error,
            error => Self::other(error),
        }
    }
}
