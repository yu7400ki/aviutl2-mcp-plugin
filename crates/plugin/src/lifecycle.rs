//! インスタンスのライフサイクル状態管理。
//!
//! `starting` → `ready` → (`busy` ↔ `ready`) → `draining` → descriptor 削除
//! の遷移を管理し、各遷移ごとに registry descriptor を原子的に更新する。

use crate::registry::RegistryWriter;
use anyhow::{Context, Result};
use aviutl2_mcp_core::{
    AuthSecret, DescriptorProject, InstanceDescriptor, InstanceId, InstanceState, ProtocolVersion,
    pipe_name_for,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

/// インスタンスのライフサイクルと descriptor 状態を管理する。
pub struct Lifecycle {
    instance_id: InstanceId,
    auth_secret: AuthSecret,
    writer: RegistryWriter,
    descriptor: Mutex<InstanceDescriptor>,
    shutdown: AtomicBool,
}

impl Lifecycle {
    /// 新しいライフサイクルを `starting` 状態で開始し、descriptor を書き込む。
    pub fn new(
        instance_id: InstanceId,
        auth_secret: AuthSecret,
        pid: u32,
        process_created_at: String,
        hwnd: Option<String>,
        started_at: String,
        writer: RegistryWriter,
    ) -> Result<Self> {
        let descriptor = InstanceDescriptor {
            schema_version: 1,
            protocol_version: ProtocolVersion::CURRENT,
            instance_id,
            pipe_name: pipe_name_for(&instance_id),
            auth_secret: auth_secret.clone(),
            pid,
            process_created_at,
            hwnd,
            started_at,
            state: InstanceState::Starting,
            project: None,
        };
        writer
            .write(&descriptor)
            .context("starting descriptor の書き込みに失敗しました")?;

        Ok(Self {
            instance_id,
            auth_secret,
            writer,
            descriptor: Mutex::new(descriptor),
            shutdown: AtomicBool::new(false),
        })
    }

    pub fn instance_id(&self) -> InstanceId {
        self.instance_id
    }

    pub fn auth_secret(&self) -> &AuthSecret {
        &self.auth_secret
    }

    pub fn state(&self) -> InstanceState {
        self.descriptor
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .state
            .clone()
    }

    /// descriptor への参照を取得する（内部更新用）。
    fn lock_descriptor(&self) -> MutexGuard<'_, InstanceDescriptor> {
        self.descriptor.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 状態を遷移し、descriptor を原子的に更新する。
    ///
    /// 許可される遷移:
    /// - `starting` → `ready`
    /// - `ready` ↔ `busy`
    /// - `ready` → `draining`
    /// - `busy` → `draining`
    pub fn transition_to(&self, new_state: InstanceState) -> Result<()> {
        if self.shutdown.load(Ordering::Acquire) {
            anyhow::bail!("shutdown 済みのため状態遷移できません");
        }

        let mut descriptor = self.lock_descriptor();
        let old_state = descriptor.state.clone();

        let allowed = match (&old_state, &new_state) {
            (InstanceState::Starting, InstanceState::Ready) => true,
            (InstanceState::Ready, InstanceState::Busy)
            | (InstanceState::Busy, InstanceState::Ready) => true,
            (InstanceState::Ready, InstanceState::Draining)
            | (InstanceState::Busy, InstanceState::Draining) => true,
            _ if old_state == new_state => return Ok(()),
            _ => false,
        };

        if !allowed {
            anyhow::bail!("無効な状態遷移です: {old_state} → {new_state}");
        }

        descriptor.state = new_state;
        self.writer.write(&descriptor).with_context(|| {
            format!(
                "descriptor の状態更新に失敗しました: {old_state} → {descriptor_state}",
                descriptor_state = descriptor.state
            )
        })?;
        Ok(())
    }

    pub fn update_project(&self, project: Option<DescriptorProject>) -> Result<()> {
        if self.shutdown.load(Ordering::Acquire) {
            anyhow::bail!("shutdown 済みのためプロジェクト情報を更新できません");
        }

        let mut descriptor = self.lock_descriptor();
        descriptor.project = project;
        self.writer
            .write(&descriptor)
            .context("プロジェクト情報の descriptor 更新に失敗しました")?;
        Ok(())
    }

    /// 終了処理を開始する。
    ///
    /// `ready`/`busy` → `draining` へ遷移し、新規要求を拒否する。
    pub fn shutdown(&self) -> Result<()> {
        // draining 遷移を先に行い、以降の新規要求を拒否する。
        let result = self.transition_to(InstanceState::Draining);
        self.shutdown.store(true, Ordering::Release);
        result
    }

    /// インスタンスを `gone` 状態に移行し、descriptor を削除する。
    pub fn mark_gone(&self) -> Result<()> {
        self.shutdown.store(true, Ordering::Release);
        {
            let mut descriptor = self.lock_descriptor();
            descriptor.state = InstanceState::Gone;
        }
        self.writer
            .remove(&self.instance_id)
            .context("descriptor の削除に失敗しました")?;
        Ok(())
    }

    pub fn descriptor(&self) -> InstanceDescriptor {
        self.lock_descriptor().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::AuthSecret;
    use std::path::PathBuf;

    fn temp_writer() -> (RegistryWriter, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "aviutl2-mcp-lifecycle-test-{}",
            InstanceId::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        // RegistryWriter::for_dir は tests 用に registry.rs で定義されている。
        // ここではその存在を利用する。
        (RegistryWriter::for_dir(dir.clone()), dir)
    }

    fn sample_identity() -> (InstanceId, AuthSecret, u32, String, Option<String>, String) {
        (
            InstanceId::new_v4(),
            AuthSecret::generate(),
            std::process::id(),
            "2026-01-01T00:00:00Z".to_string(),
            Some("0x0".to_string()),
            "2026-01-01T00:00:00Z".to_string(),
        )
    }

    #[test]
    fn lifecycle_starts_in_starting_state() {
        let (writer, dir) = temp_writer();
        let (id, secret, pid, created_at, hwnd, started_at) = sample_identity();
        let lifecycle =
            Lifecycle::new(id, secret, pid, created_at, hwnd, started_at, writer).unwrap();

        assert_eq!(lifecycle.state(), InstanceState::Starting);
        assert!(dir.join(format!("{}.json", id)).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transition_starting_to_ready() {
        let (writer, dir) = temp_writer();
        let (id, secret, pid, created_at, hwnd, started_at) = sample_identity();
        let lifecycle =
            Lifecycle::new(id, secret, pid, created_at, hwnd, started_at, writer).unwrap();

        lifecycle.transition_to(InstanceState::Ready).unwrap();
        assert_eq!(lifecycle.state(), InstanceState::Ready);

        let content = std::fs::read_to_string(dir.join(format!("{}.json", id))).unwrap();
        let parsed: aviutl2_mcp_core::InstanceDescriptor = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.state, InstanceState::Ready);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transition_ready_busy_roundtrip() {
        let (writer, dir) = temp_writer();
        let (id, secret, pid, created_at, hwnd, started_at) = sample_identity();
        let lifecycle =
            Lifecycle::new(id, secret, pid, created_at, hwnd, started_at, writer).unwrap();
        lifecycle.transition_to(InstanceState::Ready).unwrap();

        lifecycle.transition_to(InstanceState::Busy).unwrap();
        assert_eq!(lifecycle.state(), InstanceState::Busy);

        lifecycle.transition_to(InstanceState::Ready).unwrap();
        assert_eq!(lifecycle.state(), InstanceState::Ready);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shutdown_transitions_to_draining() {
        let (writer, dir) = temp_writer();
        let (id, secret, pid, created_at, hwnd, started_at) = sample_identity();
        let lifecycle =
            Lifecycle::new(id, secret, pid, created_at, hwnd, started_at, writer).unwrap();
        lifecycle.transition_to(InstanceState::Ready).unwrap();

        lifecycle.shutdown().unwrap();
        assert_eq!(lifecycle.state(), InstanceState::Draining);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mark_gone_removes_descriptor() {
        let (writer, dir) = temp_writer();
        let (id, secret, pid, created_at, hwnd, started_at) = sample_identity();
        let lifecycle =
            Lifecycle::new(id, secret, pid, created_at, hwnd, started_at, writer).unwrap();
        lifecycle.transition_to(InstanceState::Ready).unwrap();

        lifecycle.mark_gone().unwrap();
        assert_eq!(lifecycle.state(), InstanceState::Gone);
        assert!(!dir.join(format!("{}.json", id)).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_project_reflects_to_descriptor() {
        let (writer, dir) = temp_writer();
        let (id, secret, pid, created_at, hwnd, started_at) = sample_identity();
        let lifecycle =
            Lifecycle::new(id, secret, pid, created_at, hwnd, started_at, writer).unwrap();
        lifecycle.transition_to(InstanceState::Ready).unwrap();

        let project = DescriptorProject {
            display_name: "Test".to_string(),
            path: r"C:\test.aup".to_string(),
        };
        lifecycle.update_project(Some(project.clone())).unwrap();

        let descriptor = lifecycle.descriptor();
        assert_eq!(descriptor.project, Some(project));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_transition_fails() {
        let (writer, dir) = temp_writer();
        let (id, secret, pid, created_at, hwnd, started_at) = sample_identity();
        let lifecycle =
            Lifecycle::new(id, secret, pid, created_at, hwnd, started_at, writer).unwrap();

        assert!(lifecycle.transition_to(InstanceState::Busy).is_err());
        assert!(lifecycle.transition_to(InstanceState::Draining).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
