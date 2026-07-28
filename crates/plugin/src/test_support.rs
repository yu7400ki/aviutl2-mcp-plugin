//! テストが共有する補助。

use std::sync::{Mutex, MutexGuard};

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
