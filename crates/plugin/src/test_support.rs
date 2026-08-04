//! テストが共有する補助。

use crate::read::host::HostEffect;
use aviutl2_mcp_core::{ObjectFingerprintInput, ObjectSummary};
use std::sync::{Mutex, MutexGuard};

/// fingerprint の食い違いが運ぶ「読み直した対象の概要」の代表値。
///
/// 失敗の代表値を組み立てる箇所が複数あり、そのたびに材料を書き並べると、
/// 概要の形が変わったときに直す箇所が散らばる。
pub(crate) fn sample_object_summary() -> ObjectSummary {
    ObjectSummary::new(
        "78be92d1-c8c9-44c6-ae52-387548971468",
        ObjectFingerprintInput {
            scene_id: 0,
            layer: 1,
            frame_start: 100,
            frame_end: 200,
            name: Some("立ち絵"),
            alias: "[1:100]",
        },
    )
}

/// 配下 effect の設定値・有効状態・ロック状態を含む alias を組み立てる。
///
/// ホストが返す alias は配下 effect の設定値・有効状態・ロック状態を本文へ含み、
/// effect を変えれば alias も追随する。オブジェクトの fingerprint は alias だけを
/// 材料にするため、フェイクの alias がこの性質を持たなければ「effect を変えると
/// 対象の同一性が変わる」ことを検証できない。
///
/// effect の各値は表示のためだけに文字列へ写す。求める性質は「値が変われば
/// alias も変わる」ことだけであり、書式そのものに意味は無い。
pub(crate) fn alias_with_effects(base: &str, effects: &[HostEffect]) -> String {
    let mut alias = base.to_string();
    for (position, effect) in effects.iter().enumerate() {
        alias.push_str(&format!(
            "\n[Object.{position}] effect.name={}",
            effect.name
        ));
        for item in &effect.items {
            alias.push_str(&format!(" / {}={:?}", item.name, item.value));
        }
        // 有効状態とロックは既定から外れた節にだけ印が付く。
        if !effect.enabled {
            alias.push_str(" / display.disable=1");
        }
        if effect.locked {
            alias.push_str(" / display.lock=1");
        }
    }
    alias
}

/// tracing イベントの出力先として使う共有バッファ。
#[derive(Clone, Default)]
pub(crate) struct LogCapture(std::sync::Arc<Mutex<Vec<u8>>>);

impl LogCapture {
    fn contents(&self) -> String {
        let buffer = self.0.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&buffer).into_owned()
    }
}

impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> aviutl2::tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// `f` の実行中に発行された tracing イベントを集めて返す。
///
/// 出力先はこのスレッドの subscriber に限られるため、ホストのログ設定にも
/// 他のテストにも影響しない。
pub(crate) fn capture_logs(f: impl FnOnce()) -> String {
    let capture = LogCapture::default();
    let subscriber = aviutl2::tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    capture.contents()
}

/// panic フックの差し替えを直列化する。
///
/// フックはプロセス全体で 1 つしかなく、テストは並列に走る。複数のテストが
/// 同時に差し替えると、復元の順序によっては何も出力しないフックが残り、
/// 無関係なテストの panic を診断できなくなる。
static PANIC_HOOK: Mutex<()> = Mutex::new(());

/// 既定フックによる標準エラーへの出力を抑えて `f` を実行する。
///
/// panic を捕捉して黙らせるのは `f` の内側だけである。`f` から漏れた panic は
/// フックを復元してから呼び出し側へ伝え直すため、通常どおり診断できる。
///
/// 入れ子にはできない。単一のロックで直列化しており、内側の呼び出しは
/// ロックの解放を待ち続ける。
pub(crate) fn with_silent_panic_hook<T>(f: impl FnOnce() -> T) -> T {
    let guard = lock();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(previous);
    drop(guard);

    match result {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// 毒されたロックもそのまま使う。保護しているのは差し替えの区間だけで、
/// 一貫性を保つべき状態を持たない。
fn lock() -> MutexGuard<'static, ()> {
    PANIC_HOOK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silenced_panic_is_caught_and_escaping_panic_is_propagated() {
        let caught = with_silent_panic_hook(|| {
            std::panic::catch_unwind(|| panic!("内側で panic させます")).is_err()
        });
        assert!(caught, "内側の panic が捕捉されていません");

        let escaped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_silent_panic_hook(|| panic!("漏れる panic"));
        }));
        assert!(escaped.is_err(), "漏れた panic が握り潰されています");
    }
}
