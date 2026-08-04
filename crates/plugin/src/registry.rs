//! plugin 側の registry 書き込み。
//!
//! `%LOCALAPPDATA%\AviUtl2Mcp\instances\` 以下に descriptor JSON を
//! 原子的に書き込み、DACL を設定する。
//!
//! 失敗の説明は上位でログへ出るため、絶対パスと完全な識別子を含めない。
//! どの descriptor で失敗したかは [`crate::redact`] を通した形で添える。

use crate::redact::{descriptor_file, instance_id as redact_instance_id};
use anyhow::{Context, Result};
use aviutl2_mcp_core::{InstanceDescriptor, InstanceId};
use aviutl2_mcp_win::{create_protected_directory, create_protected_file};
use std::io::Write;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    DeleteFileW, FlushFileBuffers, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    ReplaceFileW,
};
use windows::core::PCWSTR;

/// registry への descriptor 書き込みを担当する。
///
/// 書き込みは同一インスタンスに対する複数の並行更新を防ぐため内部で直列化する。
pub struct RegistryWriter {
    root_dir: PathBuf,
    registry_dir: PathBuf,
    lock: Mutex<()>,
}

/// registry と一時成果物が共有する基底ディレクトリ（`%LOCALAPPDATA%\AviUtl2Mcp`）。
///
/// 探索の入口である descriptor も、プロセス外へ渡す一時成果物も、ここを起点に
/// 置く。基底を 2 か所で別々に求めると、片方だけが移ったときに server が
/// 成果物を見つけられなくなる。
pub fn discovery_root() -> Result<PathBuf> {
    let local_app_data =
        std::env::var("LOCALAPPDATA").context("LOCALAPPDATA 環境変数が取得できませんでした")?;
    Ok(PathBuf::from(local_app_data).join("AviUtl2Mcp"))
}

impl RegistryWriter {
    /// 既定の registry ディレクトリ（`%LOCALAPPDATA%\AviUtl2Mcp\instances`）を使用して
    /// 新しい `RegistryWriter` を作成する。
    pub fn new() -> Result<Self> {
        let root_dir = discovery_root()?;
        let registry_dir = root_dir.join("instances");

        let writer = Self {
            root_dir,
            registry_dir,
            lock: Mutex::new(()),
        };
        writer.ensure_directories()?;
        Ok(writer)
    }

    /// registry ルートと instances ディレクトリを保護 DACL 付きで用意する。
    ///
    /// ルート（`%LOCALAPPDATA%\AviUtl2Mcp`）にも instances と同じ DACL を設定する。
    /// ルートを保護しなければ、instances 自体の DACL を持たない第三者が
    /// ルート側の権限でサブディレクトリごと差し替えられるため、保護対象は
    /// ルートから連続している必要がある。
    ///
    /// 既存のディレクトリは検証にとどめ、DACL を書き換えない。想定と異なれば
    /// 失敗させる。**その場合 instance は登録されず、MCP からは見えない。**
    /// 保証できない場所へ descriptor を置くより、見えないほうがよい。
    ///
    /// `%LOCALAPPDATA%` 自体は作成対象にも保護の対象にも含めない。
    fn ensure_directories(&self) -> Result<()> {
        create_protected_directory(&self.root_dir)
            .context("registry ルートディレクトリを用意できませんでした")?;
        create_protected_directory(&self.registry_dir)
            .context("registry ディレクトリを用意できませんでした")?;
        Ok(())
    }

    /// `descriptor` を registry に原子的に書き込む。
    pub fn write(&self, descriptor: &InstanceDescriptor) -> Result<()> {
        let _lock = self.lock.lock().unwrap_or_else(|e| e.into_inner());

        if !self.registry_dir.exists() {
            self.ensure_directories()?;
        }

        let json = serde_json::to_string_pretty(descriptor)
            .context("descriptor の JSON 直列化に失敗しました")?;

        let tmp_path = self.temp_path(&descriptor.instance_id);
        let target_path = self.target_path(&descriptor.instance_id);
        let _temp_file = TempFileGuard(&tmp_path);

        let mut file = create_protected_file(&tmp_path).with_context(|| {
            format!(
                "一時ファイルを作成できませんでした: descriptor={}",
                descriptor_file(&tmp_path)
            )
        })?;
        file.write_all(json.as_bytes()).with_context(|| {
            format!(
                "一時ファイルへの書き込みに失敗しました: descriptor={}",
                descriptor_file(&tmp_path)
            )
        })?;

        unsafe {
            let raw_handle = file.as_raw_handle();
            FlushFileBuffers(HANDLE(raw_handle))
                .ok()
                .context("ファイルバッファの flush に失敗しました")?;
        }
        mem::drop(file);

        atomic_replace(&tmp_path, &target_path).with_context(|| {
            format!(
                "descriptor の原子的置換に失敗しました: instance_id={}",
                redact_instance_id(&descriptor.instance_id)
            )
        })?;

        Ok(())
    }

    /// 指定したインスタンスの descriptor を削除する。
    ///
    /// ファイルが存在しない場合は無視する。
    pub fn remove(&self, instance_id: &InstanceId) -> Result<()> {
        let _lock = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.target_path(instance_id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| {
                format!(
                    "descriptor の削除に失敗しました: instance_id={}",
                    redact_instance_id(instance_id)
                )
            }),
        }
    }

    fn target_path(&self, instance_id: &InstanceId) -> PathBuf {
        self.registry_dir.join(format!("{}.json", instance_id))
    }

    fn temp_path(&self, instance_id: &InstanceId) -> PathBuf {
        let random = uuid::Uuid::new_v4().to_string();
        self.registry_dir
            .join(format!("{}.json.{}.tmp", instance_id, random))
    }
}

struct TempFileGuard<'a>(&'a Path);

impl<'a> Drop for TempFileGuard<'a> {
    fn drop(&mut self) {
        if self.0.exists() {
            let wide = to_wide(self.0);
            unsafe {
                let _ = DeleteFileW(PCWSTR(wide.as_ptr()));
            }
        }
    }
}

fn atomic_replace(temp_path: &Path, target_path: &Path) -> Result<()> {
    let temp_wide = to_wide(temp_path);
    let target_wide = to_wide(target_path);

    unsafe {
        if target_path.exists() {
            ReplaceFileW(
                PCWSTR(target_wide.as_ptr()),
                PCWSTR(temp_wide.as_ptr()),
                None,
                windows::Win32::Storage::FileSystem::REPLACE_FILE_FLAGS(0),
                None,
                None,
            )
            .ok()
            .context("ReplaceFileW に失敗しました")?;
        } else {
            MoveFileExW(
                PCWSTR(temp_wide.as_ptr()),
                PCWSTR(target_wide.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
            .ok()
            .context("MoveFileExW に失敗しました")?;
        }
    }
    Ok(())
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
    use aviutl2_mcp_core::{
        AuthSecret, DescriptorProject, InstanceDescriptor, InstanceId, InstanceState,
        ProtocolVersion, pipe_name_for,
    };

    fn temp_registry_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aviutl2-mcp-registry-test-{}",
            InstanceId::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sample_descriptor() -> InstanceDescriptor {
        let id = InstanceId::new_v4();
        InstanceDescriptor {
            schema_version: 1,
            protocol_version: ProtocolVersion::CURRENT,
            instance_id: id,
            pipe_name: pipe_name_for(&id),
            auth_secret: AuthSecret::generate(),
            pid: std::process::id(),
            process_created_at: "2026-01-01T00:00:00.0000000Z".to_string(),
            hwnd: None,
            started_at: "2026-01-01T00:00:00.0000000Z".to_string(),
            state: InstanceState::Starting,
            project: None,
        }
    }

    fn sample_descriptor_with_project(id: InstanceId) -> InstanceDescriptor {
        InstanceDescriptor {
            schema_version: 1,
            protocol_version: ProtocolVersion::CURRENT,
            instance_id: id,
            pipe_name: pipe_name_for(&id),
            auth_secret: AuthSecret::generate(),
            pid: std::process::id(),
            process_created_at: "2026-01-01T00:00:00.0000000Z".to_string(),
            hwnd: Some("0x0".to_string()),
            started_at: "2026-01-01T00:00:00.0000000Z".to_string(),
            state: InstanceState::Ready,
            project: Some(DescriptorProject {
                display_name: "Test Project".to_string(),
                path: r"C:\test.aup".to_string(),
            }),
        }
    }

    #[test]
    fn write_and_read_roundtrip() {
        let dir = temp_registry_root();
        let writer = RegistryWriter::for_dir(dir.clone());
        let descriptor = sample_descriptor();

        writer.write(&descriptor).unwrap();

        let path = writer.target_path(&descriptor.instance_id);
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: InstanceDescriptor = serde_json::from_str(&content).unwrap();
        assert_eq!(descriptor, parsed);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_existing_descriptor() {
        let dir = temp_registry_root();
        let writer = RegistryWriter::for_dir(dir.clone());
        let id = InstanceId::new_v4();
        let first = sample_descriptor_with_project(id);
        writer.write(&first).unwrap();

        let mut second = sample_descriptor_with_project(id);
        second.state = InstanceState::Busy;
        second.project.as_mut().unwrap().display_name = "Updated".to_string();
        writer.write(&second).unwrap();

        let path = writer.target_path(&id);
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: InstanceDescriptor = serde_json::from_str(&content).unwrap();
        assert_eq!(second, parsed);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_descriptor() {
        let dir = temp_registry_root();
        let writer = RegistryWriter::for_dir(dir.clone());
        let descriptor = sample_descriptor();

        writer.write(&descriptor).unwrap();
        let path = writer.target_path(&descriptor.instance_id);
        assert!(path.exists());

        writer.remove(&descriptor.instance_id).unwrap();
        assert!(!path.exists());

        // 削除済みの場合もエラーにならないこと
        writer.remove(&descriptor.instance_id).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn root_and_instances_directories_are_protected() {
        let dir = temp_registry_root();
        let writer = RegistryWriter::for_dir(dir.clone());

        writer.write(&sample_descriptor()).unwrap();

        aviutl2_mcp_win::test_support::assert_protected_dacl(&dir);
        aviutl2_mcp_win::test_support::assert_protected_dacl(&dir.join("instances"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn written_json_contains_auth_secret_field() {
        let dir = temp_registry_root();
        let writer = RegistryWriter::for_dir(dir.clone());
        let descriptor = sample_descriptor();

        writer.write(&descriptor).unwrap();

        let path = writer.target_path(&descriptor.instance_id);
        let content = std::fs::read_to_string(&path).unwrap();
        // auth_secret は descriptor 内部情報として JSON に含まれる
        assert!(content.contains("auth_secret"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    impl RegistryWriter {
        /// `root_dir` を registry ルートに見立て、その配下の `instances` を
        /// 書き込み先とする（本番と同じ 2 階層構成）。
        pub(crate) fn for_dir(root_dir: PathBuf) -> Self {
            let registry_dir = root_dir.join("instances");
            Self {
                root_dir,
                registry_dir,
                lock: Mutex::new(()),
            }
        }
    }
}
