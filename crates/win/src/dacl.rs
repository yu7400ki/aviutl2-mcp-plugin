//! 保護された DACL の組み立て。
//!
//! ここが組み立てるのは「新規作成時に与える DACL」だけである。既存の
//! オブジェクトへ設定し直す口は持たない。

use crate::ProtectedDirError;
use std::ffi::c_void;
use std::io;
use windows::Win32::Foundation::{CloseHandle, FALSE, GENERIC_ALL, HANDLE};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAceEx, CONTAINER_INHERIT_ACE, CopySid,
    CreateWellKnownSid, GetLengthSid, GetTokenInformation, InitializeAcl,
    InitializeSecurityDescriptor, IsValidSid, OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser, WELL_KNOWN_SID_TYPE,
    WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// 許可する主体の数（現在のユーザー・SYSTEM・Administrators）。
pub(crate) const SUBJECT_COUNT: usize = 3;

/// 保護された DACL を持つ `SECURITY_ATTRIBUTES` を所有する。
///
/// 破棄されるまで内部のセキュリティ記述子・ACL・SID バッファを保持する。
/// [`Self::as_ptr`] が返すポインタはこれらを指すため、本体より長く持ち越せない。
pub struct ProtectedSecurityAttributes {
    /// セキュリティ記述子本体。`attrs.lpSecurityDescriptor` から指される。
    _sd: Vec<u8>,
    /// ACL 本体。`_sd` の DACL として指される。
    _acl: Vec<u8>,
    /// ACE が参照する SID のバッファ。
    _sids: Vec<Vec<u8>>,
    attrs: SECURITY_ATTRIBUTES,
}

impl ProtectedSecurityAttributes {
    /// 現在のユーザー・SYSTEM・Administrators だけに `GENERIC_ALL` を許可する
    /// セキュリティ属性を組み立てる。
    ///
    /// ACE は継承可能であるため、これを与えて作ったディレクトリの中に作られる
    /// ファイルも同じ 3 主体だけに開かれる。継承そのものは無効化するため、
    /// 親ディレクトリの ACE は入らない。
    pub fn new() -> Result<Self, ProtectedDirError> {
        Self::for_sids(protected_sids()?)
    }

    /// 与えた SID だけを許可するセキュリティ属性を組み立てる。
    pub(crate) fn for_sids(sids: Vec<Vec<u8>>) -> Result<Self, ProtectedDirError> {
        // SAFETY: ACL と記述子のバッファはこの関数内で確保し、長さは ACE の
        // 大きさと SID の長さから求めている。`sids` は移動して本構造体が
        // 保持し続けるため、`sid_ptrs` の指す先は構造体より長く生きる。
        unsafe {
            let sid_ptrs: Vec<PSID> = sids
                .iter()
                .map(|sid| PSID(sid.as_ptr().cast::<c_void>() as *mut c_void))
                .collect();
            for sid in &sid_ptrs {
                if !IsValidSid(*sid).as_bool() {
                    return Err(io::Error::other("無効な SID が生成されました").into());
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
                _acl: acl,
                _sids: sids,
                attrs,
            })
        }
    }

    /// `CreateDirectoryW` / `CreateFileW` などへ渡す `SECURITY_ATTRIBUTES` への
    /// ポインタを返す。
    pub fn as_ptr(&self) -> *const SECURITY_ATTRIBUTES {
        &self.attrs
    }
}

/// 許可する 3 主体の SID を組み立てる。
///
/// 並びは DACL の中でも検証でも同じ意味を持つ。現在のユーザー・SYSTEM・
/// Administrators の順である。
pub(crate) fn protected_sids() -> Result<Vec<Vec<u8>>, ProtectedDirError> {
    // SAFETY: 返り値のバッファはそのまま呼び出し元へ渡り、`PSID` を作るのは
    // 同じ `Vec` を保持したままの [`ProtectedSecurityAttributes::for_sids`] と
    // 検証だけである。バッファを縮めたり作り直したりする経路は無い。
    unsafe {
        Ok(vec![
            current_user_sid()?,
            well_known_sid(WinLocalSystemSid)?,
            well_known_sid(WinBuiltinAdministratorsSid)?,
        ])
    }
}

/// 現在のプロセストークンが持つユーザー SID を複製する。
///
/// # Safety
///
/// 返り値は Win32 が SID として解釈する生のバイト列である。呼び出し側は
/// バッファの内容を変えずに保持し、そこから作った `PSID` の寿命をバッファの
/// 寿命の内側へ収めること。
pub(crate) unsafe fn current_user_sid() -> Result<Vec<u8>, ProtectedDirError> {
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
/// 返り値の扱いに課される約束は [`current_user_sid`] と同じである。
pub(crate) unsafe fn well_known_sid(
    kind: WELL_KNOWN_SID_TYPE,
) -> Result<Vec<u8>, ProtectedDirError> {
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

/// Win32 の失敗を `io::Error` へ包む。
///
/// `windows::core::Error` は Win32 のエラーを `HRESULT`（`0x8007_XXXX`）として
/// 保持するため、そのまま `from_raw_os_error` へ渡すと `raw_os_error` が生の
/// Win32 コードと一致しない。FACILITY_WIN32 の場合は元のコードへ戻す。
///
/// 戻すことで `ErrorKind` が意味を持つ。「作れなかった」理由（権限が無い・
/// 親が無い・使用中）は呼び出し元の対処が違うため、`Other` へ畳まない。
pub(crate) fn to_io_error(err: windows::core::Error) -> io::Error {
    let hresult = err.code().0 as u32;
    if hresult & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((hresult & 0xFFFF) as i32)
    } else {
        io::Error::other(err)
    }
}
