//! 生成物の書き出しと、撤回したぶんの削除。
//!
//! # 書き込みの規律
//!
//! - **毎回の起動で照合し、内容が違うときだけ書く。** 無変更で触らないのは、
//!   クライアントがファイルの更新時刻を見ている場合に無駄な再読み込みを
//!   起こさないためである。
//! - **原子的に置き換える。** 複数の AviUtl2 が同時に起動しても、半分書けた
//!   ファイルを読ませない。
//! - **生成物は plugin が所有する。** 手で編集しても次回の起動で戻る。
//!
//! # 実行中の exe は上書きできない
//!
//! クライアントが `bin\` の複製から server を起動している最中に AviUtl2 を
//! 起動すると、置き換えが共有違反で失敗する。**諦めてログへ残す。** その場に
//! 居る server は古いビルドであり、IPC の握手が protocol version を照合して
//! 拒否する——**静かに動き続ける経路は無い。** 次に AviUtl2 を起動した時点で
//! 複製し直される。
//!
//! **リネームで退かしてから書く手は採らない。** 退かした残骸の削除を次回起動へ
//! 持ち越すことになり、保護ディレクトリへ寿命の違うファイルを 1 種類増やす。
//! 得るものは、握手が既に検出している失敗を 1 回早めることだけである。
//!
//! # 消す対象
//!
//! **走査して消さない。** 消すのは [`super::GenerationPlan::removed_paths`] が
//! 返す列挙だけであり、ディレクトリは空になったときだけ削る。ルートを再帰削除
//! する経路を持たない。

use super::{GenerationPlan, SERVER_EXECUTABLE};
use crate::atomic_file::write_protected_atomic;
use anyhow::{Context, Result};
use aviutl2_mcp_core::settings::AgentPluginSettings;
use aviutl2_mcp_win::create_protected_directory;
use std::path::{Path, PathBuf};

/// 現在の設定に合わせて生成物を揃える。
///
/// **失敗しても呼び出し元を止めない。** 起動時も設定画面も同じ関数を通り、
/// 起動時のものは差分の是正であって別の判断を持たない。
///
/// 触れるのは設定の読み書き口と自 DLL のパスだけである。plugin の singleton
/// にも編集ハンドルにも触れないため、設定画面のコールバックから呼べる。
pub fn sync() {
    if let Err(e) = sync_now() {
        tracing::warn!("agent plugin の生成に失敗しました: {e:#}");
    }
}

fn sync_now() -> Result<()> {
    let root = crate::registry::discovery_root()?;
    let settings = crate::settings::current();
    let source = crate::identity::plugin_directory().map(|dir| dir.join(SERVER_EXECUTABLE));
    apply(&root, &settings.agent_plugin(), source.as_deref())
}

/// `root` の下を `settings` の内容へ揃える。
///
/// `source` は server の実行体の複製元である。**隣に無ければ manifest を
/// 1 つも書かない**——存在しないパスを指す marketplace を置くほうが、
/// marketplace が無いことより悪い。クライアントは起動に失敗するまで気付けない。
pub(crate) fn apply(
    root: &Path,
    settings: &AgentPluginSettings,
    source: Option<&Path>,
) -> Result<()> {
    let plan = super::plan(settings);
    prune(root, &plan);
    if plan.is_empty() {
        return Ok(());
    }

    let Some(source) = source.filter(|path| path.is_file()) else {
        tracing::warn!(
            "{SERVER_EXECUTABLE} が plugin の隣に見つからないため agent plugin を生成しません"
        );
        return Ok(());
    };

    create_protected_directory(root).context("生成先のルートを用意できませんでした")?;
    for file in plan.files() {
        write_if_changed(root, &file.path, file.contents.as_bytes())
            .with_context(|| format!("{} を書き出せませんでした", file.path))?;
    }
    if let Some(destination) = plan.executable() {
        copy_executable(root, destination, source);
    }
    Ok(())
}

/// 内容が違うときだけ原子的に置き換える。
fn write_if_changed(root: &Path, relative: &str, contents: &[u8]) -> Result<()> {
    let target = root.join(relative);
    if std::fs::read(&target).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    let parent = target
        .parent()
        .context("生成物の置き場所を決められませんでした")?;
    ensure_directories(root, parent)?;
    write_protected_atomic(&temp_path(&target), &target, contents)
}

/// server の実行体を複製する。
///
/// **置き換えに失敗したら諦めてログへ残す。** 実行中の exe は上書きできない。
fn copy_executable(root: &Path, relative: &str, source: &Path) {
    let target = root.join(relative);
    let contents = match std::fs::read(source) {
        Ok(contents) => contents,
        Err(e) => {
            tracing::warn!("{SERVER_EXECUTABLE} を読み取れませんでした: {e}");
            return;
        }
    };
    if std::fs::read(&target).is_ok_and(|current| current == contents) {
        return;
    }
    let Some(parent) = target.parent() else {
        return;
    };
    if let Err(e) = ensure_directories(root, parent) {
        tracing::warn!("{SERVER_EXECUTABLE} の置き場所を用意できませんでした: {e:#}");
        return;
    }
    if let Err(e) = write_protected_atomic(&temp_path(&target), &target, &contents) {
        tracing::warn!(
            "{SERVER_EXECUTABLE} の複製を差し替えられませんでした。\
             次に AviUtl2 を起動した時点で複製し直します: {e:#}"
        );
    }
}

/// 撤回したぶんを消す。
///
/// **ディレクトリは空になったときだけ削る。** 空でなければ何もしない——
/// そこに居るのは生成器が置いたものではない。
fn prune(root: &Path, plan: &GenerationPlan) {
    for relative in plan.removed_paths() {
        let target = root.join(&relative);
        match std::fs::remove_file(&target) {
            Ok(()) => tracing::info!("agent plugin の生成物を削除しました: {relative}"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("{relative} を削除できませんでした: {e}"),
        }
    }
    for relative in plan.pruned_directories() {
        // 空でなければ失敗する。**それが期待する振る舞いである。**
        if std::fs::remove_dir(root.join(&relative)).is_ok() {
            tracing::debug!("空になったディレクトリを削除しました: {relative}");
        }
    }
}

/// `root` から `directory` までを保護 DACL 付きで用意する。
///
/// 既存のディレクトリは検証にとどめ、DACL を書き換えない
/// （[`create_protected_directory`]）。
fn ensure_directories(root: &Path, directory: &Path) -> Result<()> {
    let relative = directory
        .strip_prefix(root)
        .context("生成先がルートの外にあります")?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        create_protected_directory(&current).with_context(|| {
            format!("{} を用意できませんでした", component.as_os_str().display())
        })?;
    }
    Ok(())
}

/// 同一ディレクトリに採る一時ファイルのパス。
fn temp_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!("{name}.{}.tmp", uuid::Uuid::new_v4()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// 一時的な生成先と、複製元に見立てた実行体。
    ///
    /// **複製元を生成先の外へ置く。** 中に置くと、生成先のルートが保護 DACL を
    /// 持たない状態で先に作られてしまい、本番と違う経路を測ることになる。
    struct Sandbox {
        root: PathBuf,
        source: PathBuf,
    }

    impl Sandbox {
        fn new(executable: &[u8]) -> Self {
            let base = std::env::temp_dir().join(format!(
                "aviutl2-mcp-agent-plugin-test-{}",
                uuid::Uuid::new_v4()
            ));
            let source_dir = base.join("plugin");
            std::fs::create_dir_all(&source_dir).unwrap();
            let source = source_dir.join(SERVER_EXECUTABLE);
            std::fs::write(&source, executable).unwrap();
            Self {
                root: base.join("AviUtl2Mcp"),
                source,
            }
        }

        /// 生成先以下の全ファイルを、根からの相対パスで集める。
        fn tree(&self) -> BTreeSet<String> {
            tree(&self.root)
        }

        fn apply(&self, settings: &AgentPluginSettings) {
            apply(&self.root, settings, Some(&self.source)).unwrap();
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            if let Some(base) = self.root.parent() {
                let _ = std::fs::remove_dir_all(base);
            }
        }
    }

    /// `root` 以下の全ファイルを、根からの相対パスで集める。
    fn tree(root: &Path) -> BTreeSet<String> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, out);
                } else {
                    out.insert(
                        path.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        let mut found = BTreeSet::new();
        walk(root, root, &mut found);
        found
    }

    fn all_on() -> AgentPluginSettings {
        AgentPluginSettings {
            generate: true,
            claude: true,
            agent_plugins: true,
            skill: true,
        }
    }

    #[test]
    fn a_full_generation_lands_exactly_the_planned_paths() {
        let sandbox = Sandbox::new(b"server");

        sandbox.apply(&all_on());

        assert_eq!(
            sandbox.tree(),
            super::super::plan(&all_on()).written_paths()
        );
    }

    #[test]
    fn nothing_is_written_when_the_executable_is_not_next_to_the_plugin() {
        // 存在しないパスを指す marketplace を置くほうが、marketplace が無い
        // ことより悪い。**クライアントは起動に失敗するまで気付けない。**
        let sandbox = Sandbox::new(b"server");
        std::fs::remove_file(&sandbox.source).unwrap();

        apply(&sandbox.root, &all_on(), None).unwrap();
        assert!(sandbox.tree().is_empty(), "{:?}", sandbox.tree());

        apply(&sandbox.root, &all_on(), Some(&sandbox.source)).unwrap();
        assert!(sandbox.tree().is_empty(), "{:?}", sandbox.tree());
    }

    #[test]
    fn an_unchanged_file_is_not_touched_again() {
        // クライアントがファイルの更新時刻を見ている場合に、無変更で
        // 再読み込みを強いない。
        let sandbox = Sandbox::new(b"server");
        sandbox.apply(&all_on());

        let marker = sandbox.root.join("README.md");
        let before = std::fs::metadata(&marker).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        sandbox.apply(&all_on());
        let after = std::fs::metadata(&marker).unwrap().modified().unwrap();

        assert_eq!(before, after, "無変更のファイルを書き直しました");
    }

    #[test]
    fn editing_a_generated_file_is_undone_on_the_next_pass() {
        let sandbox = Sandbox::new(b"server");
        sandbox.apply(&all_on());

        let manifest = sandbox.root.join("plugins/aviutl2/mcp.json");
        std::fs::write(&manifest, b"{}").unwrap();
        sandbox.apply(&all_on());

        assert_eq!(
            std::fs::read_to_string(&manifest).unwrap(),
            super::super::manifest::spec_mcp(),
            "手で編集した内容が戻りませんでした"
        );
    }

    #[test]
    fn a_rebuilt_executable_is_copied_again() {
        let sandbox = Sandbox::new(b"old build");
        sandbox.apply(&all_on());

        std::fs::write(&sandbox.source, b"new build").unwrap();
        sandbox.apply(&all_on());

        assert_eq!(
            std::fs::read(
                sandbox
                    .root
                    .join("plugins/aviutl2/bin")
                    .join(SERVER_EXECUTABLE)
            )
            .unwrap(),
            b"new build",
            "複製が起動ごとに揃いませんでした"
        );
    }

    #[test]
    fn dropping_the_consent_removes_the_tree_and_spares_everything_else() {
        // **倒せば消える。** 同居している設定・instance・成果物には触れない。
        let sandbox = Sandbox::new(b"server");
        sandbox.apply(&all_on());

        let root = &sandbox.root;
        std::fs::write(root.join("settings.json"), b"{}").unwrap();
        std::fs::create_dir_all(root.join("instances")).unwrap();
        std::fs::write(root.join("instances/keep.json"), b"{}").unwrap();
        std::fs::create_dir_all(root.join("artifacts")).unwrap();
        std::fs::write(root.join("artifacts/keep.png"), b"png").unwrap();

        sandbox.apply(&AgentPluginSettings::default());

        assert_eq!(
            sandbox.tree(),
            BTreeSet::from([
                "artifacts/keep.png".to_string(),
                "instances/keep.json".to_string(),
                "settings.json".to_string(),
            ]),
            "撤回で残骸が出たか、消してはならないものを消しました"
        );
        assert!(root.is_dir(), "ルートごと消えました");
        for directory in ["plugins", ".claude-plugin", ".agents"] {
            assert!(!root.join(directory).exists(), "{directory} が残りました");
        }
    }

    #[test]
    fn dropping_one_dialect_leaves_the_other_intact() {
        let sandbox = Sandbox::new(b"server");
        sandbox.apply(&all_on());

        let settings = AgentPluginSettings {
            claude: false,
            ..all_on()
        };
        sandbox.apply(&settings);

        assert_eq!(
            sandbox.tree(),
            super::super::plan(&settings).written_paths()
        );
        assert!(!sandbox.root.join(".claude-plugin").exists());
        assert!(
            sandbox
                .root
                .join(".agents/plugins/marketplace.json")
                .is_file()
        );
    }

    #[test]
    fn the_generated_files_carry_the_protected_dacl() {
        let sandbox = Sandbox::new(b"server");
        sandbox.apply(&all_on());

        let root = &sandbox.root;
        aviutl2_mcp_win::test_support::assert_protected_dacl(&root.join("plugins/aviutl2"));
        aviutl2_mcp_win::test_support::assert_protected_dacl(
            &root.join("plugins/aviutl2/mcp.json"),
        );

        // 一時ファイルを残さない。
        let leftovers: Vec<String> = sandbox
            .tree()
            .into_iter()
            .filter(|path| path.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "一時ファイルが残っています: {leftovers:?}"
        );
    }
}
