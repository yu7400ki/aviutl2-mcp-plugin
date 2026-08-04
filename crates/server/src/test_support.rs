//! テストが共有する補助。

use std::sync::{Arc, Mutex};

/// tracing イベントの出力先として使う共有バッファ。
#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl LogCapture {
    fn contents(&self) -> String {
        let buffer = self.0.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&buffer).into_owned()
    }
}

thread_local! {
    /// このスレッドの出力先。捕捉していないスレッドの出力は捨てる。
    static SINK: std::cell::RefCell<Option<LogCapture>> = const { std::cell::RefCell::new(None) };
}

/// 呼び出したスレッドの [`SINK`] へ書き出す writer。
#[derive(Clone, Default)]
struct ThreadLocalWriter;

impl std::io::Write for ThreadLocalWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        SINK.with(|sink| {
            if let Some(capture) = sink.borrow().as_ref() {
                capture
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ThreadLocalWriter {
    type Writer = ThreadLocalWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// `f` の実行中に、このスレッドが発行した tracing イベントを集めて返す。
///
/// subscriber はプロセス全体の既定として一度だけ設置する。**スレッドごとの
/// 既定に頼ると、他のテストが並行して走っている間に callsite の判定が
/// 「誰も購読していない」で固定され、何も捕捉できないことがある。** 出力先は
/// スレッドごとに分かれるため、他のテストの記録は混ざらない。
pub(crate) fn capture_logs<T>(f: impl FnOnce() -> T) -> (T, String) {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(ThreadLocalWriter)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("捕捉用の subscriber を設置できます");
    });

    let capture = LogCapture::default();
    SINK.with(|sink| *sink.borrow_mut() = Some(capture.clone()));
    let value = f();
    SINK.with(|sink| *sink.borrow_mut() = None);
    (value, capture.contents())
}
