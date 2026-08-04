//! 既存のオブジェクトが持つ DACL の検証。
//!
//! 見るのは「広すぎないこと」だけである。3 主体以外へ許可を与える ACE が無い
//! ことと、継承が無効であることを確かめる。**権限が狭すぎることは見ない**——
//! 危険なのは開いていることであり、狭さは後続の入出力が失敗として教える。

use crate::ProtectedDirError;
use crate::dacl::{SUBJECT_COUNT, protected_sids, to_io_error, to_wide};
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::path::Path;
use std::ptr;
use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
    DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, GetFileSecurityW,
    GetSecurityDescriptorControl, GetSecurityDescriptorDacl, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_PROTECTED,
};
use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
use windows::core::{BOOL, PCWSTR};

/// 読み出した自己相対形式のセキュリティ記述子。
///
/// 内部のポインタを先頭からの相対位置で持つため、同じ DACL は同じバイト列に
/// なる。書き換えの有無を読み比べで確かめられる形である。
pub(crate) struct SecurityDescriptor {
    /// 記述子の本体。Win32 は 4 バイト境界を要求するため語単位で確保する。
    words: Vec<u32>,
}

impl SecurityDescriptor {
    fn zeroed(len: usize) -> Self {
        Self {
            words: vec![0u32; len.div_ceil(size_of::<u32>())],
        }
    }

    fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        PSECURITY_DESCRIPTOR(self.words.as_ptr() as *mut c_void)
    }

    fn as_mut_ptr(&mut self) -> PSECURITY_DESCRIPTOR {
        PSECURITY_DESCRIPTOR(self.words.as_mut_ptr().cast::<c_void>())
    }

    /// 記述子をバイト列として返す。
    ///
    /// 末尾の詰め物まで含むが、確保時に 0 で埋めているため内容は記述子だけで
    /// 決まる。
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn bytes(&self) -> &[u8] {
        // SAFETY: `words` は 0 で初期化済みであり、長さは確保した大きさに等しい。
        unsafe {
            std::slice::from_raw_parts(
                self.words.as_ptr().cast::<u8>(),
                std::mem::size_of_val(&self.words[..]),
            )
        }
    }
}

/// 対象の DACL が保護されていることを検証する。
///
/// 継承が無効であること、および許可される主体が 3 主体に収まっていることを
/// 確かめる。いずれかを満たさない場合は [`ProtectedDirError::NotProtected`]
/// を返す。**対象は書き換えない。**
pub(crate) fn verify_protected_dacl(path: &Path) -> Result<(), ProtectedDirError> {
    let descriptor = read_security_descriptor(path)?;
    if !dacl_is_protected(&descriptor)? {
        return Err(ProtectedDirError::NotProtected);
    }
    allowed_subjects(&descriptor)?;
    Ok(())
}

/// 対象のセキュリティ記述子を読み出す。
pub(crate) fn read_security_descriptor(
    path: &Path,
) -> Result<SecurityDescriptor, ProtectedDirError> {
    let wide = to_wide(path);
    let mut needed = 0u32;
    // SAFETY: `wide` は NUL 終端したパスである。長さの問い合わせであり、
    // 記述子の書き込み先は渡さない。
    let queried = unsafe {
        GetFileSecurityW(
            PCWSTR(wide.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            None,
            0,
            &mut needed,
        )
    };
    // 長さの問い合わせはバッファ不足で失敗する。それ以外の失敗は対象へ
    // 届いていないことを意味するため、長さを持たないまま先へ進まない。
    if let Err(e) = queried.ok()
        && e.code() != ERROR_INSUFFICIENT_BUFFER.into()
    {
        return Err(to_io_error(e).into());
    }

    let length = needed;
    let mut descriptor = SecurityDescriptor::zeroed(length as usize);
    // SAFETY: 事前問い合わせで求めた長さのバッファを渡す。
    unsafe {
        GetFileSecurityW(
            PCWSTR(wide.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            Some(descriptor.as_mut_ptr()),
            length,
            &mut needed,
        )
        .ok()
        .map_err(to_io_error)?;
    }
    Ok(descriptor)
}

/// DACL の継承が無効であるかを返す。
pub(crate) fn dacl_is_protected(
    descriptor: &SecurityDescriptor,
) -> Result<bool, ProtectedDirError> {
    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: `descriptor` は有効なセキュリティ記述子を保持している。
    unsafe { GetSecurityDescriptorControl(descriptor.as_ptr(), &mut control, &mut revision) }
        .map_err(to_io_error)?;
    Ok(control & SE_DACL_PROTECTED.0 != 0)
}

/// 許可されている主体が 3 主体に収まっていることを確かめる。
///
/// 収まっていない場合は [`ProtectedDirError::NotProtected`] を返す。戻り値は
/// 3 主体のうち許可する ACE が見つかったものである。**すべて揃っていることは
/// 要求しない**——1 つ欠けても封じ込めは破れず、欠けていて困るなら後続の
/// 入出力が失敗として教える。
///
/// ACE の数は見ない。既存のオブジェクトへ設定された DACL は、継承可能な ACE が
/// 「対象自身に効くもの」と「子へ継承させるもの」へ分割され得るためであり、
/// 主体の集合は分割で変わらない。アクセスマスクも見ない——`GENERIC_ALL` は
/// 格納時に写像されて要求した値と一致せず、狭いことは危険ではない。拒否型の
/// ACE も見ない。封じ込めを弱めないためである。
pub(crate) fn allowed_subjects(
    descriptor: &SecurityDescriptor,
) -> Result<[bool; SUBJECT_COUNT], ProtectedDirError> {
    let expected = protected_sids()?;
    let mut seen = [false; SUBJECT_COUNT];

    // SAFETY: `acl` と各 ACE は `descriptor` が保持するバッファ内を指すため、
    // その生存中だけ参照する。`expected` の各バッファは有効な SID である。
    unsafe {
        let mut present = BOOL::default();
        let mut acl = ptr::null_mut::<ACL>();
        let mut defaulted = BOOL::default();
        GetSecurityDescriptorDacl(descriptor.as_ptr(), &mut present, &mut acl, &mut defaulted)
            .map_err(to_io_error)?;
        // DACL を持たない対象は誰にでも開いている。
        if !present.as_bool() || acl.is_null() {
            return Err(ProtectedDirError::NotProtected);
        }

        let mut info = MaybeUninit::<ACL_SIZE_INFORMATION>::uninit();
        GetAclInformation(
            acl,
            info.as_mut_ptr().cast::<c_void>(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
        .map_err(to_io_error)?;
        let info = info.assume_init();

        for index in 0..info.AceCount {
            let mut ace = ptr::null_mut();
            GetAce(acl, index, &mut ace).map_err(to_io_error)?;
            // 型を見てから許可型の形へ読み替える。ACE の大きさは型ごとに
            // 異なるため、先に読み替えると許可型でない ACE を許可型の大きさで
            // 読むことになる。
            let header = &*(ace as *const ACE_HEADER);
            if header.AceType as u32 != ACCESS_ALLOWED_ACE_TYPE {
                continue;
            }
            let ace = &*(ace as *const ACCESS_ALLOWED_ACE);
            let subject = PSID(ptr::addr_of!(ace.SidStart) as *mut c_void);
            let matched = expected.iter().position(|sid| {
                EqualSid(subject, PSID(sid.as_ptr().cast::<c_void>() as *mut c_void)).is_ok()
            });
            match matched {
                Some(matched) => seen[matched] = true,
                None => return Err(ProtectedDirError::NotProtected),
            }
        }
    }

    Ok(seen)
}
