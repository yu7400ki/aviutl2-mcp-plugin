//! Windows セキュリティ記述子と DACL 構築の adapter。
//!
//! registry ディレクトリと descriptor ファイルに対し、現在のユーザー・SYSTEM・
//! Administrators のみにフルコントロールを許可し、継承 ACE を持ち込まない
//! 保護された DACL を設定する。

use anyhow::{Context, Result};
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Foundation::{CloseHandle, FALSE, GENERIC_ALL, HANDLE};
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
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE,
    FILE_SHARE_MODE,
};
use windows::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::PCWSTR;

/// 保護された DACL を持つ `SECURITY_ATTRIBUTES` を所有する。
///
/// 破棄されるまで内部のセキュリティ記述子と ACL、SID バッファを保持する。
pub struct ProtectedSecurityAttributes {
    /// セキュリティ記述子本体。`attrs.lpSecurityDescriptor` から指される。
    #[allow(dead_code)]
    sd: Vec<u8>,
    acl: Vec<u8>,
    _sids: Vec<Vec<u8>>,
    attrs: SECURITY_ATTRIBUTES,
}

impl ProtectedSecurityAttributes {
    /// 現在ユーザー・SYSTEM・Administrators のみに `GENERIC_ALL` を許可する
    /// セキュリティ属性を構築する。
    pub fn new() -> Result<Self> {
        unsafe {
            let sids = vec![
                current_user_sid().context("現在のユーザー SID を取得できませんでした")?,
                well_known_sid(WinLocalSystemSid).context("SYSTEM SID を作成できませんでした")?,
                well_known_sid(WinBuiltinAdministratorsSid)
                    .context("Administrators SID を作成できませんでした")?,
            ];

            let sid_ptrs: Vec<PSID> = sids
                .iter()
                .map(|sid| PSID(sid.as_ptr().cast::<c_void>() as *mut c_void))
                .collect();

            for sid in &sid_ptrs {
                if !IsValidSid(*sid).as_bool() {
                    anyhow::bail!("無効な SID が生成されました");
                }
            }

            let mut acl_size = std::mem::size_of::<ACL>() as u32;
            for sid in &sid_ptrs {
                acl_size += (std::mem::size_of::<ACCESS_ALLOWED_ACE>() - std::mem::size_of::<u32>())
                    as u32
                    + GetLengthSid(*sid);
            }

            let mut acl = vec![0u8; acl_size as usize];
            InitializeAcl(acl.as_mut_ptr().cast::<ACL>(), acl_size, ACL_REVISION)
                .context("ACL を初期化できませんでした")?;

            for sid in &sid_ptrs {
                AddAccessAllowedAceEx(
                    acl.as_mut_ptr().cast::<ACL>(),
                    ACL_REVISION,
                    CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE,
                    GENERIC_ALL.0,
                    *sid,
                )
                .context("ACE を ACL に追加できませんでした")?;
            }

            let mut sd = vec![0u8; std::mem::size_of::<SECURITY_DESCRIPTOR>()];
            InitializeSecurityDescriptor(
                PSECURITY_DESCRIPTOR(sd.as_mut_ptr().cast::<c_void>()),
                SECURITY_DESCRIPTOR_REVISION,
            )
            .context("セキュリティ記述子を初期化できませんでした")?;

            SetSecurityDescriptorDacl(
                PSECURITY_DESCRIPTOR(sd.as_mut_ptr().cast::<c_void>()),
                true,
                Some(acl.as_mut_ptr().cast::<ACL>()),
                false,
            )
            .context("DACL を設定できませんでした")?;

            SetSecurityDescriptorControl(
                PSECURITY_DESCRIPTOR(sd.as_mut_ptr().cast::<c_void>()),
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            )
            .context("DACL 継承無効化に失敗しました")?;

            let attrs = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.as_mut_ptr().cast::<c_void>(),
                bInheritHandle: FALSE,
            };

            Ok(Self {
                sd,
                acl,
                _sids: sids,
                attrs,
            })
        }
    }

    /// `CreateDirectoryW` / `CreateFileW` に渡す `SECURITY_ATTRIBUTES` へのポインタを返す。
    pub fn as_ptr(&self) -> *const SECURITY_ATTRIBUTES {
        &self.attrs
    }
}

/// 保護された DACL を設定してディレクトリを作成する。
///
/// 既存の場合は DACL を再設定する。
///
/// 失敗の説明に対象パスを含めない。失敗は上位でログへ出るため、利用者の
/// ディレクトリ構成を残さない。どの対象で失敗したかは呼び出し元が匿名化した
/// 形で添える。
pub fn create_protected_directory(path: &Path) -> Result<()> {
    let wide = to_wide(path);
    if path.exists() {
        set_protected_dacl(path).context("既存ディレクトリの DACL 設定に失敗しました")?;
        return Ok(());
    }

    let sa = ProtectedSecurityAttributes::new()?;
    unsafe {
        CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(sa.as_ptr()))
            .context("保護されたディレクトリを作成できませんでした")?;
    }
    Ok(())
}

/// 保護された DACL を持つ新規ファイルを作成し、書き込み用の `File` を返す。
///
/// 失敗の説明に対象パスを含めない理由は [`create_protected_directory`] と同じ。
pub fn create_protected_file(path: &Path) -> Result<std::fs::File> {
    let wide = to_wide(path);
    let sa = ProtectedSecurityAttributes::new()?;
    unsafe {
        let handle = CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            Some(sa.as_ptr()),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .context("保護されたファイルを作成できませんでした")?;

        use std::os::windows::io::FromRawHandle;
        Ok(std::fs::File::from_raw_handle(handle.0))
    }
}

/// 既存のファイルまたはディレクトリに保護された DACL を設定する。
pub fn set_protected_dacl(path: &Path) -> Result<()> {
    let wide = to_wide(path);
    let sa = ProtectedSecurityAttributes::new()?;
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
        .context("保護された DACL の設定に失敗しました")?;
    }
    Ok(())
}

fn current_user_sid() -> Result<Vec<u8>> {
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .ok()
            .context("現在のプロセストークンを開けませんでした")?;
    }

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
        .ok()
        .context("トークン情報を取得できませんでした")?;

        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let sid_len = GetLengthSid(user.User.Sid);
        let mut sid = vec![0u8; sid_len as usize];
        CopySid(
            sid_len,
            PSID(sid.as_mut_ptr().cast::<c_void>()),
            user.User.Sid,
        )
        .ok()
        .context("現在のユーザー SID をコピーできませんでした")?;
        Ok(sid)
    };

    unsafe {
        let _ = CloseHandle(token);
    }
    result
}

fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> Result<Vec<u8>> {
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
        .ok()
        .context("well-known SID を作成できませんでした")?;
        Ok(buf)
    }
}

fn to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 指定パスの DACL が保護済みであることを検証する。
///
/// 検証内容は次の 3 点。
///
/// - 全 ACE が許可型であること
/// - 全 ACE の主体が現在ユーザー・SYSTEM・Administrators のいずれかであり、
///   3 主体すべてが含まれること
/// - 継承が無効化（`SE_DACL_PROTECTED`）されていること
///
/// ACE 数そのものは検証しない。`SECURITY_ATTRIBUTES` で新規作成した場合は
/// 与えた ACL がそのまま格納されて 3 個になるが、既存オブジェクトへ
/// `SetNamedSecurityInfoW` で設定した場合、コンテナとオブジェクトで
/// 意味の異なる汎用アクセス権を持つ継承可能 ACE が、対象自身に効く ACE と
/// 子へ継承させる `INHERIT_ONLY` の ACE へ分割されて 6 個になる。
/// どちらも許可される主体は変わらないため、主体の集合で検証する。
///
/// 検証に失敗した場合は panic する。
#[cfg(test)]
pub(crate) fn assert_protected_dacl(path: &Path) {
    use std::mem::MaybeUninit;
    use std::ptr;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::GetNamedSecurityInfoW;
    use windows::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL_SIZE_INFORMATION, AclSizeInformation, EqualSid, GetAce,
        GetAclInformation, GetSecurityDescriptorControl,
    };
    use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

    let expected_sids = [
        ("現在のユーザー", current_user_sid().unwrap()),
        ("SYSTEM", well_known_sid(WinLocalSystemSid).unwrap()),
        (
            "Administrators",
            well_known_sid(WinBuiltinAdministratorsSid).unwrap(),
        ),
    ];
    let mut seen = [false; 3];

    let wide = to_wide(path);
    // SAFETY: wide は NUL 終端済みで、以降の呼び出し中は生存する。acl と各 ACE は
    // sd が指すバッファ内を指すため、sd を LocalFree するまでの間だけ参照する。
    // expected_sids の各バッファは有効な SID であり、この関数の間は生存する。
    unsafe {
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
        .unwrap_or_else(|e| panic!("DACL の取得に失敗しました: path={}, {e}", path.display()));

        let mut info = MaybeUninit::<ACL_SIZE_INFORMATION>::uninit();
        GetAclInformation(
            acl,
            info.as_mut_ptr().cast::<c_void>(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
        .expect("ACL 情報の取得に失敗しました");
        let info = info.assume_init();
        assert!(
            info.AceCount >= 3,
            "ACE は 3 主体分以上必要です: path={}, count={}",
            path.display(),
            info.AceCount
        );

        let mut control = 0u16;
        let mut revision = 0u32;
        GetSecurityDescriptorControl(sd, &mut control, &mut revision)
            .expect("セキュリティ記述子コントロールの取得に失敗しました");
        assert_ne!(
            control & SE_DACL_PROTECTED.0,
            0,
            "DACL 継承は無効化されている必要があります: path={}",
            path.display()
        );

        for i in 0..info.AceCount {
            let mut ace = ptr::null_mut();
            GetAce(acl, i, &mut ace).expect("ACE の取得に失敗しました");
            let ace = &*(ace as *const ACCESS_ALLOWED_ACE);
            assert_eq!(
                ace.Header.AceType as u32,
                ACCESS_ALLOWED_ACE_TYPE,
                "ACE は許可型である必要があります: path={}",
                path.display()
            );

            let ace_sid = PSID(ptr::addr_of!(ace.SidStart) as *mut c_void);
            let matched = expected_sids
                .iter()
                .position(|(_, sid)| {
                    EqualSid(ace_sid, PSID(sid.as_ptr().cast::<c_void>() as *mut c_void)).is_ok()
                })
                .unwrap_or_else(|| {
                    panic!(
                        "想定外の主体を許可する ACE があります: path={}, index={i}",
                        path.display()
                    )
                });
            seen[matched] = true;
        }

        let _ = LocalFree(Some(HLOCAL(sd.0)));
    }

    for (idx, (name, _)) in expected_sids.iter().enumerate() {
        assert!(
            seen[idx],
            "{name} を許可する ACE がありません: path={}",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aviutl2-mcp-security-test-{}",
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn protected_directory_has_dacl() {
        let dir = temp_dir();
        create_protected_directory(&dir).unwrap();

        assert!(dir.exists());
        assert_protected_dacl(&dir);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn protected_file_has_dacl() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("test.json");

        {
            let mut file = create_protected_file(&file_path).unwrap();
            std::io::Write::write_all(&mut file, b"{}").unwrap();
        }

        assert!(file_path.exists());
        assert_protected_dacl(&file_path);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_directory_dacl_is_reapplied() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();

        create_protected_directory(&dir).unwrap();
        assert_protected_dacl(&dir);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
