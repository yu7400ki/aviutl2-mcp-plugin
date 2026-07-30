//! 現在のユーザーへ限定した DACL を持つディレクトリの作成。
//!
//! 成果物には利用者のプロジェクトの内容が写る。既定の継承 ACE をそのまま
//! 受け取ると、親ディレクトリの権限次第で他のユーザーへ読み取りが開くため、
//! 現在のユーザー SID・SYSTEM・Administrators の 3 主体だけを許可し、
//! 継承を無効化した DACL を明示的に与える。
//!
//! **同じ基底の下に置かれるディレクトリは、どのプロセスが作っても同一の方式で
//! 保護されていなければならない。** registry と descriptor を作る側（plugin）も
//! 同じ 3 主体・同じ継承の無効化で DACL を組み立てる。片方だけを緩めると、
//! 同じ基底の中に他のユーザーが読めるディレクトリが混ざり、基底全体の保護が
//! そこで途切れる。方式を変えるときは、必ず両方を同時に変えること。

use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, FALSE, GENERIC_ALL, HANDLE};
use windows::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAceEx, CONTAINER_INHERIT_ACE, CopySid,
    CreateWellKnownSid, DACL_SECURITY_INFORMATION, GetLengthSid, GetTokenInformation,
    InitializeAcl, InitializeSecurityDescriptor, IsValidSid, OBJECT_INHERIT_ACE,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
    SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser, WELL_KNOWN_SID_TYPE,
    WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::PCWSTR;

/// 保護された DACL を持つディレクトリを作成する。
///
/// 既に存在する場合は DACL を設定し直す。作成した ACE は継承可能であるため、
/// 以降このディレクトリへ作成されるファイルも同じ 3 主体だけに開かれる。
///
/// 存在の有無は作成を試みた結果で判定する。先に問い合わせてから作ると、
/// その間に別のプロセスが同じディレクトリを作った場合に失敗してしまう。
/// 基底とその直下は複数のプロセスが共有するため、ほぼ同時の起動で必ず踏む。
///
/// 失敗の説明に対象パスを含めない。利用者のディレクトリ構成をログへ残さないため、
/// どの対象で失敗したかは呼び出し元が匿名化した形で添える。
pub fn create_protected_directory(path: &Path) -> io::Result<()> {
    let wide = to_wide(path);
    let sa = ProtectedSecurityAttributes::new()?;
    // SAFETY: `wide` は NUL 終端したパスであり、`sa` は本呼び出しの間生存する。
    match unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(sa.as_ptr())) } {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERROR_ALREADY_EXISTS.into() => set_protected_dacl(path),
        Err(e) => Err(to_io_error(e)),
    }
}

/// 既存のディレクトリへ保護された DACL を設定する。
fn set_protected_dacl(path: &Path) -> io::Result<()> {
    let wide = to_wide(path);
    let sa = ProtectedSecurityAttributes::new()?;
    // SAFETY: `wide` は NUL 終端したパスであり、`sa` が保持する ACL は
    // 本呼び出しの間生存する。
    unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(sa.acl.as_ptr().cast::<ACL>()),
            None,
        )
        .ok()
    }
    .map_err(to_io_error)
}

/// 保護された DACL を持つ `SECURITY_ATTRIBUTES` を所有する。
///
/// 破棄されるまで内部のセキュリティ記述子・ACL・SID バッファを保持する。
/// `attrs` はこれらを指すため、単独で取り出して持ち越すことはできない。
struct ProtectedSecurityAttributes {
    /// セキュリティ記述子本体。`attrs.lpSecurityDescriptor` から指される。
    _sd: Vec<u8>,
    /// ACL 本体。`_sd` の DACL として指され、DACL の再設定でも直接渡す。
    acl: Vec<u8>,
    /// ACE が参照する SID のバッファ。
    _sids: Vec<Vec<u8>>,
    attrs: SECURITY_ATTRIBUTES,
}

impl ProtectedSecurityAttributes {
    fn new() -> io::Result<Self> {
        // SAFETY: 各 API へ渡すバッファはこの関数内で確保し、必要な長さを
        // 事前問い合わせで求めている。生成した SID は本構造体が保持し続ける。
        unsafe {
            let sids = vec![
                current_user_sid()?,
                well_known_sid(WinLocalSystemSid)?,
                well_known_sid(WinBuiltinAdministratorsSid)?,
            ];

            let sid_ptrs: Vec<PSID> = sids
                .iter()
                .map(|sid| PSID(sid.as_ptr().cast::<c_void>() as *mut c_void))
                .collect();
            for sid in &sid_ptrs {
                if !IsValidSid(*sid).as_bool() {
                    return Err(io::Error::other("無効な SID が生成されました"));
                }
            }

            let mut acl_size = size_of::<ACL>() as u32;
            for sid in &sid_ptrs {
                acl_size += (size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>()) as u32
                    + GetLengthSid(*sid);
            }

            let mut acl = vec![0u8; acl_size as usize];
            InitializeAcl(acl.as_mut_ptr().cast::<ACL>(), acl_size, ACL_REVISION)
                .map_err(to_io_error)?;
            for sid in &sid_ptrs {
                AddAccessAllowedAceEx(
                    acl.as_mut_ptr().cast::<ACL>(),
                    ACL_REVISION,
                    CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE,
                    GENERIC_ALL.0,
                    *sid,
                )
                .map_err(to_io_error)?;
            }

            let mut sd = vec![0u8; size_of::<SECURITY_DESCRIPTOR>()];
            InitializeSecurityDescriptor(
                PSECURITY_DESCRIPTOR(sd.as_mut_ptr().cast::<c_void>()),
                SECURITY_DESCRIPTOR_REVISION,
            )
            .map_err(to_io_error)?;
            SetSecurityDescriptorDacl(
                PSECURITY_DESCRIPTOR(sd.as_mut_ptr().cast::<c_void>()),
                true,
                Some(acl.as_mut_ptr().cast::<ACL>()),
                false,
            )
            .map_err(to_io_error)?;
            SetSecurityDescriptorControl(
                PSECURITY_DESCRIPTOR(sd.as_mut_ptr().cast::<c_void>()),
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            )
            .map_err(to_io_error)?;

            let attrs = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.as_mut_ptr().cast::<c_void>(),
                bInheritHandle: FALSE,
            };

            Ok(Self {
                _sd: sd,
                acl,
                _sids: sids,
                attrs,
            })
        }
    }

    /// `CreateDirectoryW` へ渡す `SECURITY_ATTRIBUTES` へのポインタを返す。
    fn as_ptr(&self) -> *const SECURITY_ATTRIBUTES {
        &self.attrs
    }
}

/// 現在のプロセストークンが持つユーザー SID を複製する。
///
/// # Safety
///
/// Win32 API を直接呼ぶ。呼び出し側は返り値のバッファを SID として扱うこと。
unsafe fn current_user_sid() -> io::Result<Vec<u8>> {
    let mut token = HANDLE::default();
    // SAFETY: `token` はスタック上の有効な書き込み先であり、取得したハンドルは
    // この関数の末尾で必ず閉じる。
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(to_io_error)?;

    // SAFETY: 長さの事前問い合わせで確保したバッファへ書き込ませ、その内容を
    // `TOKEN_USER` として読む。`CopySid` の複製先は同じ長さで確保している。
    let result = unsafe {
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            len,
            &mut len,
        )
        .map_err(to_io_error)?;

        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let sid_len = GetLengthSid(user.User.Sid);
        let mut sid = vec![0u8; sid_len as usize];
        CopySid(
            sid_len,
            PSID(sid.as_mut_ptr().cast::<c_void>()),
            user.User.Sid,
        )
        .map_err(to_io_error)?;
        Ok(sid)
    };

    // SAFETY: `token` はこの関数が単独で所有しており、ここでのみ閉じる。
    unsafe {
        let _ = CloseHandle(token);
    }
    result
}

/// well-known SID を生成する。
///
/// # Safety
///
/// Win32 API を直接呼ぶ。呼び出し側は返り値のバッファを SID として扱うこと。
unsafe fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Vec<u8>> {
    // SAFETY: 長さの事前問い合わせで確保したバッファへ書き込ませる。
    unsafe {
        let mut len = 0u32;
        let _ = CreateWellKnownSid(kind, None, None, &mut len);
        let mut buf = vec![0u8; len as usize];
        CreateWellKnownSid(
            kind,
            None,
            Some(PSID(buf.as_mut_ptr().cast::<c_void>())),
            &mut len,
        )
        .map_err(to_io_error)?;
        Ok(buf)
    }
}

fn to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Win32 の失敗を `io::Error` へ包む。
///
/// DACL の設定失敗に対して呼び出し側が種別ごとの分岐を持たないため、
/// 生の Win32 コードへは還元せず、表示可能な形のまま包む。
fn to_io_error(err: windows::core::Error) -> io::Error {
    io::Error::other(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;
    use std::ptr;
    use uuid::Uuid;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::GetNamedSecurityInfoW;
    use windows::Win32::Security::{
        ACL_SIZE_INFORMATION, AclSizeInformation, EqualSid, GetAce, GetAclInformation,
        GetSecurityDescriptorControl,
    };
    use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

    /// DACL が 3 主体だけを許可していることを検証する。
    ///
    /// `require_protected` が真であれば、継承が無効化されていることも検証する。
    /// 継承の無効化はそれを設定した対象の属性であり、そこから継承を受けた
    /// ファイルには付かない。ファイルについて言えるのは、許可される主体が
    /// 親から継承した 3 主体だけであることである。
    ///
    /// ACE 数そのものは検証しない。新規作成では与えた ACL がそのまま格納されるが、
    /// 既存ディレクトリへ設定し直した場合、継承可能な ACE が対象自身に効くものと
    /// 子へ継承させるものへ分割される。許可される主体は変わらないため、
    /// 主体の集合で検証する。
    fn assert_allows_only_expected_subjects(path: &Path, require_protected: bool) {
        // SAFETY: `wide` は NUL 終端済みで呼び出し中は生存する。`acl` と各 ACE は
        // `sd` が指すバッファ内を指すため、`LocalFree` までの間だけ参照する。
        unsafe {
            let expected = [
                ("現在のユーザー", current_user_sid().unwrap()),
                ("SYSTEM", well_known_sid(WinLocalSystemSid).unwrap()),
                (
                    "Administrators",
                    well_known_sid(WinBuiltinAdministratorsSid).unwrap(),
                ),
            ];
            let mut seen = [false; 3];

            let wide = to_wide(path);
            let mut acl = ptr::null_mut::<ACL>();
            let mut sd = PSECURITY_DESCRIPTOR(ptr::null_mut());
            GetNamedSecurityInfoW(
                PCWSTR(wide.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(&mut acl),
                None,
                &mut sd,
            )
            .ok()
            .expect("DACL を取得できません");

            if require_protected {
                let mut control = 0u16;
                let mut revision = 0u32;
                GetSecurityDescriptorControl(sd, &mut control, &mut revision)
                    .expect("セキュリティ記述子コントロールを取得できません");
                assert_ne!(
                    control & SE_DACL_PROTECTED.0,
                    0,
                    "DACL の継承が無効化されていません"
                );
            }

            let mut info = MaybeUninit::<ACL_SIZE_INFORMATION>::uninit();
            GetAclInformation(
                acl,
                info.as_mut_ptr().cast::<c_void>(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
            .expect("ACL 情報を取得できません");
            let info = info.assume_init();

            for i in 0..info.AceCount {
                let mut ace = ptr::null_mut();
                GetAce(acl, i, &mut ace).expect("ACE を取得できません");
                let ace = &*(ace as *const ACCESS_ALLOWED_ACE);
                assert_eq!(
                    ace.Header.AceType as u32, ACCESS_ALLOWED_ACE_TYPE,
                    "許可型でない ACE があります"
                );
                let ace_sid = PSID(ptr::addr_of!(ace.SidStart) as *mut c_void);
                let matched = expected
                    .iter()
                    .position(|(_, sid)| {
                        EqualSid(ace_sid, PSID(sid.as_ptr().cast::<c_void>() as *mut c_void))
                            .is_ok()
                    })
                    .expect("想定外の主体を許可する ACE があります");
                seen[matched] = true;
            }

            let _ = LocalFree(Some(HLOCAL(sd.0)));

            for (idx, (name, _)) in expected.iter().enumerate() {
                assert!(seen[idx], "{name} を許可する ACE がありません");
            }
        }
    }

    fn temp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("aviutl2-mcp-protected-dir-{}", Uuid::new_v4()))
    }

    #[test]
    fn new_directory_is_protected() {
        let dir = temp_dir();
        create_protected_directory(&dir).unwrap();
        assert_allows_only_expected_subjects(&dir, true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_directory_is_reprotected() {
        // 作成が「既に存在する」で失敗する経路を通る。別のプロセスが先に
        // 作った場合と同じ経路であり、失敗にせず DACL を設定し直す。
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        create_protected_directory(&dir).unwrap();
        assert_allows_only_expected_subjects(&dir, true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_created_inside_inherit_the_restriction() {
        // 成果物のファイルへ個別に DACL を設定していないため、継承した ACE が
        // ファイル単位の保護の根拠になる。
        let dir = temp_dir();
        create_protected_directory(&dir).unwrap();
        let file = dir.join("artifact.png");
        std::fs::write(&file, b"payload").unwrap();
        assert_allows_only_expected_subjects(&file, false);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
