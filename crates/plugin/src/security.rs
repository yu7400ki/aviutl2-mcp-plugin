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
pub fn create_protected_directory(path: &Path) -> Result<()> {
    let wide = to_wide(path);
    if path.exists() {
        set_protected_dacl(path).with_context(|| {
            format!(
                "既存ディレクトリの DACL 設定に失敗しました: {}",
                path.display()
            )
        })?;
        return Ok(());
    }

    let sa = ProtectedSecurityAttributes::new()?;
    unsafe {
        CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(sa.as_ptr())).with_context(|| {
            format!(
                "保護されたディレクトリを作成できませんでした: {}",
                path.display()
            )
        })?;
    }
    Ok(())
}

/// 保護された DACL を持つ新規ファイルを作成し、書き込み用の `File` を返す。
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
        .with_context(|| {
            format!(
                "保護された一時ファイルを作成できませんでした: {}",
                path.display()
            )
        })?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;
    use std::path::PathBuf;
    use std::ptr;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::GetNamedSecurityInfoW;
    use windows::Win32::Security::{
        ACE_HEADER, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION, GetAce,
        GetAclInformation, GetSecurityDescriptorControl,
    };
    use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

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
        verify_dacl(&dir);

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
        verify_dacl(&file_path);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn verify_dacl(path: &Path) {
        let wide = to_wide(path);
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
            .unwrap_or_else(|e| panic!("DACL の取得に失敗しました: {e}"));

            let mut info = MaybeUninit::<ACL_SIZE_INFORMATION>::uninit();
            GetAclInformation(
                acl,
                info.as_mut_ptr().cast::<c_void>(),
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
            .expect("ACL 情報の取得に失敗しました");
            let info = info.assume_init();
            assert_eq!(info.AceCount, 3, "ACE は 3 つである必要があります");

            let mut control = 0u16;
            let mut revision = 0u32;
            GetSecurityDescriptorControl(sd, &mut control, &mut revision)
                .expect("セキュリティ記述子コントロールの取得に失敗しました");
            assert_ne!(
                control & SE_DACL_PROTECTED.0,
                0,
                "DACL 継承は無効化されている必要があります"
            );

            for i in 0..info.AceCount {
                let mut ace = ptr::null_mut();
                GetAce(acl, i, &mut ace).expect("ACE の取得に失敗しました");
                let header = &*(ace as *const ACE_HEADER);
                assert_eq!(
                    header.AceType as u32, ACCESS_ALLOWED_ACE_TYPE,
                    "ACE は許可型である必要があります"
                );
            }

            let _ = LocalFree(Some(HLOCAL(sd.0)));
        }
    }
}
