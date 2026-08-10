//! レンダリング成果物をプロセスの外へ渡すためのファイル。
//!
//! # なぜ応答へ埋め込まずファイルにするのか
//!
//! 要求と応答を運ぶ枠には上限があり、1920×1080 の非圧縮 RGBA はその直下に
//! 収まるものの、応答へ載せる形へ直すと超える。より大きなシーンは直す前から
//! 超える。分割して何度も往復させる案も採らない。接続は同時に 1 本しか
//! 張れず、分割の間こちら側が数十 MiB を抱え続けることになる。ファイルなら
//! 保持は書き終えた時点で終わる。
//!
//! # 応答はパスを返さない
//!
//! 応答へ載せるのは識別子だけである。受け取る側は自分が探索に使った基底から
//! 同じ場所を組み立てる。**要求元がパスを組み立てる材料を一切持たない**ため、
//! 相対参照・装置名前空間・代替データストリームといったパスの攻撃面が、この
//! 経路には最初から存在しない。
//!
//! # ログに残さないもの
//!
//! 画像には利用者のプロジェクトの内容が写る。画像・パス・識別子はログへ
//! 残さない。残すのは大きさと結果だけである。

use crate::registry::discovery_root;
use crate::render::error::{ArtifactStage, RenderError};
use crate::render::slot::RenderedFrame;
use anyhow::{Context, Result};
use aviutl2_mcp_core::{
    ARTIFACT_MAX_BYTES, InstanceId, format_sha256, handoff_dir, handoff_file, handoff_temp_file,
};
use aviutl2_mcp_win::{create_protected_directory, create_protected_file, to_wide};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{FlushFileBuffers, MOVEFILE_WRITE_THROUGH, MoveFileExW};
use windows::core::PCWSTR;

pub use aviutl2_mcp_core::{ARTIFACT_MEDIA_TYPE, HandoffToken};

/// 書き終えた引き渡し用ファイルの申告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffArtifact {
    /// 引き渡し用ファイルの識別子。
    pub token: HandoffToken,
    /// 書き込んだバイト数。
    pub byte_length: u64,
    /// `"sha256:" + 64 桁小文字十六進`。
    pub sha256: String,
}

/// 自インスタンス専用の引き渡し用ディレクトリ。
///
/// 掃除はこのディレクトリの中だけで行う。他インスタンスのディレクトリへは
/// 触れない。触れば、生きているプロセスの成果物を消し得る。
#[derive(Debug)]
pub struct HandoffDir {
    /// 引き渡しの基底。パスは常にここから組み立てる。
    root: PathBuf,
    instance_id: InstanceId,
    /// 書き出してよい成果物の大きさの上限（符号化後のバイト数）。
    max_artifact_bytes: u64,
}

impl HandoffDir {
    /// 探索ディレクトリと同じ基底の下に、自インスタンスのディレクトリを定める。
    pub fn new(instance_id: &InstanceId) -> Result<Self> {
        let root = discovery_root()?;
        Ok(Self::under(&root, instance_id))
    }

    /// 基底を指定して自インスタンスのディレクトリを定める。
    ///
    /// 書き出しの上限は [`ARTIFACT_MAX_BYTES`] である。受け取る側が引き取れる
    /// 大きさと同じ値でなければ、書き出せても引き取れない成果物が生まれる。
    pub fn under(root: &Path, instance_id: &InstanceId) -> Self {
        Self {
            root: root.to_path_buf(),
            instance_id: *instance_id,
            max_artifact_bytes: ARTIFACT_MAX_BYTES,
        }
    }

    /// 自インスタンスの引き渡し用ディレクトリ。
    fn dir(&self) -> PathBuf {
        handoff_dir(&self.root, &self.instance_id)
    }

    /// 書き出しの上限を差し替える。
    ///
    /// 上限に掛かる経路を、実際に上限の大きさの画像を作らずに確かめるための口
    /// である。
    #[cfg(test)]
    pub(crate) fn with_max_artifact_bytes(mut self, max_artifact_bytes: u64) -> Self {
        self.max_artifact_bytes = max_artifact_bytes;
        self
    }

    /// 画像を PNG として符号化し、原子的に書き出す。
    ///
    /// 書き込みは一時ファイルへ完全に書いて flush し、同じディレクトリの中で
    /// 名前を差し替える。受け取る側が書き込み途中の状態を読む余地を消す。
    ///
    /// **符号化の結果が上限を超えていれば、ファイルを作らずに失敗を返す。**
    /// 上限は受け取る側が引き取れる大きさであり、超えたものは引き取られずに
    /// 捨てられる。書いてから捨てると、保護された DACL のファイルを作って
    /// 消すだけの入出力を払ったうえ、消し損ねが残る経路が増える。
    ///
    /// 符号化してからでなければ判定できない。PNG の大きさは画の中身で決まり、
    /// 非圧縮の大きさからは求まらない。
    pub fn write(&self, frame: &RenderedFrame) -> Result<HandoffArtifact, RenderError> {
        let encoded = encode_png(frame.width, frame.height, &frame.pixels)?;
        if encoded.len() as u64 > self.max_artifact_bytes {
            // 残すのは大きさだけである。画像そのものも識別子もログへ出さない。
            tracing::warn!(
                byte_length = encoded.len(),
                "符号化した成果物が上限を超えています"
            );
            return Err(RenderError::FrameTooLarge);
        }
        let digest = Sha256::digest(&encoded);

        let token = HandoffToken::generate();
        let target = self.artifact_path(&token);
        let temp = self.temp_path(&token);

        self.ensure_dir().map_err(|e| {
            tracing::error!("引き渡し用ディレクトリを用意できませんでした: {e:?}");
            RenderError::Artifact {
                stage: ArtifactStage::Write,
            }
        })?;

        let mut guard = TempFileGuard::arm(&temp);
        write_protected(&temp, &encoded).map_err(|e| {
            tracing::error!("引き渡し用ファイルを書き出せませんでした: {e:?}");
            RenderError::Artifact {
                stage: ArtifactStage::Write,
            }
        })?;
        atomic_rename(&temp, &target).map_err(|e| {
            tracing::error!("引き渡し用ファイルを確定できませんでした: {e:?}");
            RenderError::Artifact {
                stage: ArtifactStage::Write,
            }
        })?;
        // 名前の差し替えが済んだ時点で一時ファイルは存在しない。
        guard.disarm();

        Ok(HandoffArtifact {
            token,
            byte_length: encoded.len() as u64,
            sha256: format_sha256(&digest),
        })
    }

    /// 指定した識別子のファイルを消す。
    ///
    /// 応答の送信に失敗したときに使う。受け取る側は識別子を得ていないため、
    /// 引き取ることも掃除することもできない。
    ///
    /// 掃除の失敗は記録するだけで、要求を失敗させない。
    pub fn remove(&self, token: &HandoffToken) {
        remove_if_exists(&self.artifact_path(token));
        remove_if_exists(&self.temp_path(token));
    }

    /// `ttl` より古いファイルを消す。
    ///
    /// 正常時、ファイルは受け取る側が引き取った直後に消える。これは失敗経路の
    /// ための保険であり、新しい要求を受けたときに行う。専用のスレッドは持たない。
    ///
    /// `ttl` は設定から来る。下限は「引き取り中のファイルを消さない」ための
    /// 最低限であり、倍率を適用した描画の要求フェーズ予算を下回らない。
    pub fn sweep_expired(&self, now: SystemTime, ttl: Duration) {
        let entries = match std::fs::read_dir(self.dir()) {
            Ok(entries) => entries,
            // まだ 1 度も書いていない場合はディレクトリ自体が無い。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!("引き渡し用ディレクトリを走査できませんでした: {e}");
                return;
            }
        };
        let mut swept = 0usize;
        for entry in entries.flatten() {
            let expired = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .map(|modified| now.duration_since(modified).is_ok_and(|age| age >= ttl))
                .unwrap_or(false);
            if expired && std::fs::remove_file(entry.path()).is_ok() {
                swept += 1;
            }
        }
        if swept > 0 {
            tracing::debug!(swept, "期限切れの引き渡し用ファイルを削除しました");
        }
    }

    /// 自インスタンスのディレクトリごと消す。
    ///
    /// 終了へ向かう時点で使う。以後この instance が成果物を書くことはない。
    pub fn remove_all(&self) {
        match std::fs::remove_dir_all(self.dir()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("引き渡し用ディレクトリを削除できませんでした: {e}"),
        }
    }

    /// 保護された DACL を持つディレクトリを用意する。
    ///
    /// 基底から順に作る。基底を保護しなければ、その権限で配下ごと差し替え
    /// られるため、保護は基底から連続している必要がある。
    ///
    /// 既存のディレクトリは検証にとどめ、DACL を書き換えない。想定と異なれば
    /// この要求を失敗させる。
    fn ensure_dir(&self) -> Result<()> {
        let dir = self.dir();
        let mut ancestors: Vec<&Path> = dir.ancestors().take(3).collect();
        ancestors.reverse();
        for dir in ancestors {
            create_protected_directory(dir)
                .context("引き渡し用ディレクトリを用意できませんでした")?;
        }
        Ok(())
    }

    /// ディレクトリに残っているファイルの数。
    ///
    /// 書き出しと掃除の結果を、パスを外へ出さずに確かめるための口である。
    #[cfg(test)]
    pub(crate) fn entry_count(&self) -> usize {
        std::fs::read_dir(self.dir())
            .map(|entries| entries.flatten().count())
            .unwrap_or(0)
    }

    /// 書き出した成果物の中身。
    ///
    /// 画素が往復することを確かめるための口である。
    #[cfg(test)]
    pub(crate) fn read_artifact(&self, token: &HandoffToken) -> Option<Vec<u8>> {
        std::fs::read(self.artifact_path(token)).ok()
    }

    /// ディレクトリ内の全ファイルの更新時刻を過去へずらす。
    ///
    /// 期限切れの掃除が起きることを、実時間を待たずに確かめるための口である。
    #[cfg(test)]
    pub(crate) fn age_entries(&self, age: Duration) {
        let aged = SystemTime::now() - age;
        for entry in std::fs::read_dir(self.dir())
            .into_iter()
            .flatten()
            .flatten()
        {
            let file = std::fs::File::options()
                .write(true)
                .open(entry.path())
                .expect("引き渡し用ファイルを開けません");
            file.set_modified(aged)
                .expect("引き渡し用ファイルの更新時刻を変更できません");
        }
    }

    fn artifact_path(&self, token: &HandoffToken) -> PathBuf {
        handoff_file(&self.root, &self.instance_id, token)
    }

    fn temp_path(&self, token: &HandoffToken) -> PathBuf {
        handoff_temp_file(&self.root, &self.instance_id, token)
    }
}

/// RGBA8 を PNG として符号化する。
///
/// アルファチャンネルはそのまま残し、変換しない。ホストが乗算済みアルファを
/// 返すかどうかは確かめられておらず、推測して割り戻すと、乗算済みでなかった
/// 場合に色が壊れる。
///
/// 圧縮は速度側へ寄せる。用途は下見であり、符号化に割ける時間は短い。
fn encode_png(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>, RenderError> {
    encode_png_inner(width, height, pixels).map_err(|e| {
        tracing::error!("レンダリング結果を符号化できませんでした: {e}");
        RenderError::Artifact {
            stage: ArtifactStage::Encode,
        }
    })
}

fn encode_png_inner(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>, png::EncodingError> {
    let mut encoded = Vec::new();
    let mut encoder = png::Encoder::new(&mut encoded, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fastest);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(pixels)?;
    writer.finish()?;
    Ok(encoded)
}

/// 保護された DACL を持つ新規ファイルへ書き切って flush する。
fn write_protected(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file =
        create_protected_file(path).context("引き渡し用ファイルを作成できませんでした")?;
    file.write_all(bytes)
        .context("引き渡し用ファイルへの書き込みに失敗しました")?;
    // SAFETY: `file` は本関数が所有する有効なファイルハンドル。
    unsafe {
        FlushFileBuffers(HANDLE(file.as_raw_handle()))
            .ok()
            .context("ファイルバッファの flush に失敗しました")?;
    }
    Ok(())
}

/// 同じディレクトリの中で名前を差し替える。
///
/// 差し替え先は毎回新しい識別子であり、既存を上書きする経路を作らない。
fn atomic_rename(temp_path: &Path, target_path: &Path) -> Result<()> {
    let temp_wide = to_wide(temp_path);
    let target_wide = to_wide(target_path);
    // SAFETY: 両方の名前は NUL 終端済みで、呼び出し中は生存している。
    unsafe {
        MoveFileExW(
            PCWSTR(temp_wide.as_ptr()),
            PCWSTR(target_wide.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
        .ok()
        .context("引き渡し用ファイルの原子的な名前変更に失敗しました")?;
    }
    Ok(())
}

/// 書き込みに失敗した一時ファイルを残さない。
struct TempFileGuard<'a>(Option<&'a Path>);

impl<'a> TempFileGuard<'a> {
    fn arm(path: &'a Path) -> Self {
        Self(Some(path))
    }

    /// 名前の差し替えが済み、消すべき一時ファイルが無くなったことを伝える。
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for TempFileGuard<'_> {
    fn drop(&mut self) {
        if let Some(path) = self.0 {
            remove_if_exists(path);
        }
    }
}

/// 存在すれば消す。失敗は記録するだけで伝播させない。
fn remove_if_exists(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("引き渡し用ファイルを削除できませんでした: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::HANDOFF_DIR;

    /// 掃除の期限。実際の値は設定から来るため、ここでは 1 つ固定して使う。
    const TEST_TTL: Duration = Duration::from_secs(120);

    /// 一時的な基底ディレクトリ。
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .join(format!("aviutl2-mcp-handoff-test-{}", InstanceId::new_v4()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn dir_for(&self, instance_id: &InstanceId) -> HandoffDir {
            HandoffDir::under(&self.0, instance_id)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 幅 2・高さ 2 の不透明でない画像。
    fn sample_frame() -> RenderedFrame {
        RenderedFrame {
            frame: 7,
            width: 2,
            height: 2,
            pixels: vec![
                0xFF, 0x00, 0x00, 0xFF, // 不透明の赤
                0x00, 0xFF, 0x00, 0x80, // 半透明の緑
                0x00, 0x00, 0xFF, 0x00, // 完全に透明な青
                0xFF, 0xFF, 0xFF, 0xFF, // 不透明の白
            ],
        }
    }

    #[test]
    fn a_written_artifact_lands_at_the_agreed_place() {
        // 引き取る側は同じ基底と instance から自分で場所を組み立てる。ここでは
        // 組み立ての結果ではなく、取り決めそのものを書き下して照合する——
        // 共有の定義が変われば、書き出す側だけが動いてこの照合が落ちる。
        let root = TempRoot::new();
        let instance_id = InstanceId::new_v4();
        let handoff = root.dir_for(&instance_id);

        let artifact = handoff.write(&sample_frame()).expect("書き出しに失敗");

        let agreed = root
            .0
            .join("render")
            .join(instance_id.to_string())
            .join(format!("{}.png", artifact.token.as_str()));
        assert!(
            agreed.is_file(),
            "取り決めた場所に成果物がありません: {}",
            agreed.display()
        );
    }

    #[test]
    fn a_written_artifact_reports_its_own_length_and_digest() {
        let root = TempRoot::new();
        let instance_id = InstanceId::new_v4();
        let handoff = root.dir_for(&instance_id);

        let artifact = handoff.write(&sample_frame()).expect("書き出しに失敗");

        let path = handoff.artifact_path(&artifact.token);
        let written = std::fs::read(&path).expect("書き出したファイルを読めません");
        assert_eq!(artifact.byte_length, written.len() as u64);
        assert_eq!(artifact.sha256, format_sha256(&Sha256::digest(&written)));
        assert!(
            !handoff.temp_path(&artifact.token).exists(),
            "一時ファイルが残っています"
        );
    }

    #[test]
    fn a_written_artifact_is_a_png_that_keeps_the_alpha_channel() {
        let root = TempRoot::new();
        let handoff = root.dir_for(&InstanceId::new_v4());
        let frame = sample_frame();

        let artifact = handoff.write(&frame).expect("書き出しに失敗");
        let written = std::fs::read(handoff.artifact_path(&artifact.token)).unwrap();

        let decoder = png::Decoder::new(std::io::Cursor::new(written));
        let mut reader = decoder.read_info().expect("PNG として読めません");
        assert_eq!(reader.info().color_type, png::ColorType::Rgba);
        assert_eq!(reader.info().bit_depth, png::BitDepth::Eight);
        assert_eq!(reader.info().width, frame.width);
        assert_eq!(reader.info().height, frame.height);

        let mut decoded = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut decoded).expect("画素を読めません");
        assert_eq!(
            &decoded[..info.buffer_size()],
            &frame.pixels[..],
            "アルファを含む画素がそのまま往復しません"
        );
    }

    #[test]
    fn the_write_cap_is_the_shared_encoded_cap() {
        // 書き出す側と引き取る側が別の値を見ると、書き出せても引き取れない
        // 大きさの帯ができる。既定値が共有の上限そのものであることを固定する。
        let root = TempRoot::new();
        let handoff = root.dir_for(&InstanceId::new_v4());
        assert_eq!(handoff.max_artifact_bytes, ARTIFACT_MAX_BYTES);
    }

    #[test]
    fn an_artifact_over_the_cap_leaves_no_file_behind() {
        // 判定は書き出しの前に行う。書いてから捨てると、保護された DACL の
        // ファイルを作って消すだけの入出力を払い、消し損ねが残る経路が増える。
        let root = TempRoot::new();
        let instance_id = InstanceId::new_v4();
        let handoff = root.dir_for(&instance_id);
        let kept = handoff.write(&sample_frame()).expect("書き出しに失敗");

        // 同じディレクトリを、上限だけ小さくして見る。
        let capped = root.dir_for(&instance_id).with_max_artifact_bytes(1);
        let error = capped
            .write(&sample_frame())
            .expect_err("上限を超える成果物が書き出されました");
        assert!(matches!(error, RenderError::FrameTooLarge), "{error:?}");

        assert_eq!(
            capped.entry_count(),
            1,
            "上限を超える成果物のファイルが残りました"
        );
        assert!(
            handoff.artifact_path(&kept.token).exists(),
            "先に書いた成果物まで失われました"
        );
    }

    #[test]
    fn the_handoff_directory_is_protected() {
        let root = TempRoot::new();
        let instance_id = InstanceId::new_v4();
        let handoff = root.dir_for(&instance_id);
        let artifact = handoff.write(&sample_frame()).expect("書き出しに失敗");

        aviutl2_mcp_win::test_support::assert_protected_dacl(&root.0);
        aviutl2_mcp_win::test_support::assert_protected_dacl(&root.0.join(HANDOFF_DIR));
        aviutl2_mcp_win::test_support::assert_protected_dacl(&handoff.dir());
        aviutl2_mcp_win::test_support::assert_protected_dacl(
            &handoff.artifact_path(&artifact.token),
        );
    }

    #[test]
    fn a_removed_artifact_leaves_nothing_behind() {
        let root = TempRoot::new();
        let handoff = root.dir_for(&InstanceId::new_v4());
        let artifact = handoff.write(&sample_frame()).expect("書き出しに失敗");
        assert!(handoff.artifact_path(&artifact.token).exists());

        handoff.remove(&artifact.token);
        assert!(!handoff.artifact_path(&artifact.token).exists());

        // 2 度目も失敗しない。送信の失敗と引き取りが競合しても、掃除は要求を
        // 失敗させてはならない。
        handoff.remove(&artifact.token);
    }

    #[test]
    fn the_sweep_removes_only_files_older_than_the_ttl() {
        let root = TempRoot::new();
        let handoff = root.dir_for(&InstanceId::new_v4());
        let fresh = handoff.write(&sample_frame()).expect("書き出しに失敗");

        handoff.sweep_expired(SystemTime::now(), TEST_TTL);
        assert!(
            handoff.artifact_path(&fresh.token).exists(),
            "書いたばかりのファイルが消えました"
        );

        handoff.sweep_expired(
            SystemTime::now() + TEST_TTL + Duration::from_secs(1),
            TEST_TTL,
        );
        assert!(
            !handoff.artifact_path(&fresh.token).exists(),
            "期限を過ぎたファイルが残りました"
        );
    }

    #[test]
    fn the_sweep_never_touches_another_instance() {
        let root = TempRoot::new();
        let mine = root.dir_for(&InstanceId::new_v4());
        let other = root.dir_for(&InstanceId::new_v4());
        let my_artifact = mine.write(&sample_frame()).expect("書き出しに失敗");
        let other_artifact = other.write(&sample_frame()).expect("書き出しに失敗");

        mine.sweep_expired(
            SystemTime::now() + TEST_TTL + Duration::from_secs(1),
            TEST_TTL,
        );

        assert!(!mine.artifact_path(&my_artifact.token).exists());
        assert!(
            other.artifact_path(&other_artifact.token).exists(),
            "他インスタンスの成果物を消しました"
        );
    }

    #[test]
    fn removing_everything_only_removes_our_own_directory() {
        let root = TempRoot::new();
        let mine = root.dir_for(&InstanceId::new_v4());
        let other = root.dir_for(&InstanceId::new_v4());
        mine.write(&sample_frame()).expect("書き出しに失敗");
        let other_artifact = other.write(&sample_frame()).expect("書き出しに失敗");

        mine.remove_all();

        assert!(!mine.dir().exists());
        assert!(
            other.artifact_path(&other_artifact.token).exists(),
            "他インスタンスのディレクトリを消しました"
        );

        // 消えた後でも失敗しない。
        mine.remove_all();
    }

    #[test]
    fn writing_recreates_a_directory_that_was_removed() {
        let root = TempRoot::new();
        let handoff = root.dir_for(&InstanceId::new_v4());
        handoff.write(&sample_frame()).expect("書き出しに失敗");
        handoff.remove_all();

        let artifact = handoff
            .write(&sample_frame())
            .expect("削除後の書き出しに失敗");
        assert!(handoff.artifact_path(&artifact.token).exists());
    }

    #[test]
    fn an_inconsistent_image_fails_at_the_encoding_stage() {
        // 画素の長さが寸法と合わない画像は符号化できない。段を取り違えると、
        // 要求元は書き込み先の問題だと読む。
        let error = encode_png(2, 2, &[0u8; 8]).expect_err("符号化が成功しました");
        assert!(
            matches!(
                error,
                RenderError::Artifact {
                    stage: ArtifactStage::Encode
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn compression_is_biased_towards_speed() {
        // 圧縮率を既定へ戻すと、下見のための符号化が予算を超え得る。
        // 速度側へ寄せていることを、既定との差で確かめる。
        let frame = RenderedFrame {
            frame: 0,
            width: 64,
            height: 64,
            pixels: (0..64u32 * 64 * 4).map(|i| (i % 251) as u8).collect(),
        };
        let fast = encode_png(frame.width, frame.height, &frame.pixels).unwrap();

        let mut default_encoded = Vec::new();
        let mut encoder = png::Encoder::new(&mut default_encoded, frame.width, frame.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&frame.pixels).unwrap();
        writer.finish().unwrap();

        assert!(
            fast.len() > default_encoded.len(),
            "既定と同じかそれより小さくなりました。速度側へ寄せられていません: {} <= {}",
            fast.len(),
            default_encoded.len()
        );
    }

    #[test]
    fn the_response_carries_nothing_that_locates_the_file() {
        let root = TempRoot::new();
        let handoff = root.dir_for(&InstanceId::new_v4());
        let artifact = handoff.write(&sample_frame()).expect("書き出しに失敗");

        let reported = format!("{artifact:?}");
        assert!(
            !reported.contains(&root.0.display().to_string()),
            "申告にパスが含まれています: {reported}"
        );
        assert!(
            !reported.contains(HANDOFF_DIR),
            "申告にディレクトリ名が含まれています: {reported}"
        );
        // 識別子はファイル名そのものである。申告を構造体ごと記録しても
        // 場所を組み立てる材料が漏れないことを、書き出す側でも固定する。
        assert!(
            !reported.contains(artifact.token.as_str()),
            "申告に識別子が含まれています: {reported}"
        );
    }
}
