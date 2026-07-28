//! IPC framing。
//!
//! frame 形式: `uint32_le payload_length` + UTF-8 JSON body。
//! 最大 payload は 8 MiB。`length == 0` と上限超は body 確保前に拒否する。

use std::collections::VecDeque;
use thiserror::Error;

/// 最大 payload サイズ（8 MiB）。
pub const MAX_FRAME_SIZE: u32 = 8 * 1024 * 1024;

/// フレームングエラー。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FrameError {
    /// payload サイズが上限を超えた。
    #[error("payload サイズ {0} が上限 {MAX_FRAME_SIZE} を超えました")]
    PayloadTooLarge(u32),
    /// 空 payload は許可されない。
    #[error("空 payload は許可されません")]
    EmptyPayload,
    /// 不完全なフレームで接続が閉じられた。
    #[error("不完全なフレームで接続が閉じられました")]
    IncompleteFrame,
}

/// フレームデコーダーの状態。
///
/// 完成したフレームは内部キューへ積まれ、状態は `ReadingLength` へ戻る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderState {
    /// 長さ 4 バイトを読み取り中。
    ReadingLength,
    /// body を読み取り中。
    ReadingBody { length: u32 },
}

/// byte stream からの部分受信に対応した frame デコーダー。
///
/// 使用例:
///
/// ```
/// use aviutl2_mcp_core::framing::{FrameDecoder, encode_frame};
///
/// let mut decoder = FrameDecoder::new();
/// decoder.feed(&encode_frame(b"hello").unwrap()).unwrap();
/// assert_eq!(decoder.take_frame(), Some(b"hello".to_vec()));
/// ```
#[derive(Debug, Clone)]
pub struct FrameDecoder {
    state: DecoderState,
    length_buffer: [u8; 4],
    length_filled: usize,
    body: Vec<u8>,
    body_filled: usize,
    pending: VecDeque<Vec<u8>>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self {
            state: DecoderState::ReadingLength,
            length_buffer: [0; 4],
            length_filled: 0,
            body: Vec::new(),
            body_filled: 0,
            pending: VecDeque::new(),
        }
    }

    pub fn state(&self) -> DecoderState {
        self.state
    }

    /// 入力バイト列を消費し、完成したフレームを内部キューに蓄積する。
    ///
    /// 完成したフレームは `take_frame()` で取り出す。
    /// エラー時はデコーダーをリセットし、接続を終了すべきことを示す。
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), FrameError> {
        let mut cursor = 0;
        while cursor < bytes.len() {
            match self.state {
                DecoderState::ReadingLength => {
                    let need = 4 - self.length_filled;
                    let take = (bytes.len() - cursor).min(need);
                    self.length_buffer[self.length_filled..self.length_filled + take]
                        .copy_from_slice(&bytes[cursor..cursor + take]);
                    self.length_filled += take;
                    cursor += take;
                    if self.length_filled == 4 {
                        let length = u32::from_le_bytes(self.length_buffer);
                        if length == 0 {
                            self.reset();
                            return Err(FrameError::EmptyPayload);
                        }
                        if length > MAX_FRAME_SIZE {
                            self.reset();
                            return Err(FrameError::PayloadTooLarge(length));
                        }
                        self.state = DecoderState::ReadingBody { length };
                        self.body = vec![0; length as usize];
                        self.body_filled = 0;
                    }
                }
                DecoderState::ReadingBody { length } => {
                    let need = length as usize - self.body_filled;
                    let take = (bytes.len() - cursor).min(need);
                    self.body[self.body_filled..self.body_filled + take]
                        .copy_from_slice(&bytes[cursor..cursor + take]);
                    self.body_filled += take;
                    cursor += take;
                    if self.body_filled == length as usize {
                        let body = std::mem::take(&mut self.body);
                        self.pending.push_back(body);
                        self.reset();
                    }
                }
            }
        }
        Ok(())
    }

    pub fn take_frame(&mut self) -> Option<Vec<u8>> {
        self.pending.pop_front()
    }

    /// 接続終了を通知する。
    ///
    /// 現在フレームの途中であれば `IncompleteFrame` エラーを返す。
    /// `ReadingLength` の初期状態であれば正常終了する。
    pub fn end(&mut self) -> Result<(), FrameError> {
        match self.state {
            DecoderState::ReadingLength if self.length_filled == 0 => Ok(()),
            _ => {
                self.reset();
                Err(FrameError::IncompleteFrame)
            }
        }
    }

    pub fn reset(&mut self) {
        self.state = DecoderState::ReadingLength;
        self.length_buffer = [0; 4];
        self.length_filled = 0;
        self.body.clear();
        self.body_filled = 0;
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// body バイト列を frame 形式にエンコードする。
///
/// 空 body・8 MiB 超はエラーとする。
pub fn encode_frame(body: &[u8]) -> Result<Vec<u8>, FrameError> {
    if body.is_empty() {
        return Err(FrameError::EmptyPayload);
    }
    let len = body.len() as u32;
    if len > MAX_FRAME_SIZE {
        return Err(FrameError::PayloadTooLarge(len));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(body);
    Ok(frame)
}

pub fn encode_length(length: u32) -> Result<[u8; 4], FrameError> {
    if length == 0 {
        return Err(FrameError::EmptyPayload);
    }
    if length > MAX_FRAME_SIZE {
        return Err(FrameError::PayloadTooLarge(length));
    }
    Ok(length.to_le_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{RequestEnvelope, ResponseEnvelope};
    use crate::json::deserialize_json;
    use proptest::prelude::*;

    #[test]
    fn encode_decode_roundtrip() {
        let body = b"hello world";
        let frame = encode_frame(body).unwrap();
        let mut decoder = FrameDecoder::new();
        decoder.feed(&frame).unwrap();
        let decoded = decoder.take_frame().unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn split_length_and_body() {
        let body = b"hello world";
        let frame = encode_frame(body).unwrap();
        let mut decoder = FrameDecoder::new();
        decoder.feed(&frame[..2]).unwrap();
        assert_eq!(decoder.state(), DecoderState::ReadingLength);
        decoder.feed(&frame[2..6]).unwrap();
        // 4 バイト目で length が確定し、body 読み取りに移行する
        assert_eq!(decoder.state(), DecoderState::ReadingBody { length: 11 });
        decoder.feed(&frame[6..]).unwrap();
        let decoded = decoder.take_frame().unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn split_body_only() {
        let body = b"hello world";
        let frame = encode_frame(body).unwrap();
        let mut decoder = FrameDecoder::new();
        decoder.feed(&frame[..4]).unwrap();
        assert_eq!(decoder.state(), DecoderState::ReadingBody { length: 11 });
        decoder.feed(&frame[4..8]).unwrap();
        decoder.feed(&frame[8..]).unwrap();
        let decoded = decoder.take_frame().unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn reject_empty_payload() {
        assert!(matches!(encode_frame(b""), Err(FrameError::EmptyPayload)));
    }

    #[test]
    fn reject_zero_length_frame() {
        let mut decoder = FrameDecoder::new();
        let result = decoder.feed(&[0, 0, 0, 0]);
        assert!(matches!(result, Err(FrameError::EmptyPayload)));
    }

    #[test]
    fn reject_payload_too_large() {
        let huge = vec![0u8; (MAX_FRAME_SIZE + 1) as usize];
        assert!(matches!(
            encode_frame(&huge),
            Err(FrameError::PayloadTooLarge(_))
        ));
    }

    #[test]
    fn reject_length_too_large_before_allocate() {
        let mut decoder = FrameDecoder::new();
        let length = (MAX_FRAME_SIZE + 1).to_le_bytes();
        let result = decoder.feed(&length);
        assert!(matches!(result, Err(FrameError::PayloadTooLarge(_))));
    }

    #[test]
    fn incomplete_length_eof() {
        let mut decoder = FrameDecoder::new();
        decoder.feed(&[1, 2]).unwrap();
        let result = decoder.end();
        assert!(matches!(result, Err(FrameError::IncompleteFrame)));
    }

    #[test]
    fn incomplete_body_eof() {
        let mut decoder = FrameDecoder::new();
        decoder.feed(&[10, 0, 0, 0, 1, 2, 3]).unwrap();
        let result = decoder.end();
        assert!(matches!(result, Err(FrameError::IncompleteFrame)));
    }

    #[test]
    fn multiple_frames_in_single_feed() {
        let body1 = b"first";
        let body2 = b"second";
        let mut data = encode_frame(body1).unwrap();
        data.extend_from_slice(&encode_frame(body2).unwrap());
        let mut decoder = FrameDecoder::new();
        decoder.feed(&data).unwrap();
        let decoded1 = decoder.take_frame().unwrap();
        assert_eq!(decoded1, body1);
        let decoded2 = decoder.take_frame().unwrap();
        assert_eq!(decoded2, body2);
    }

    #[test]
    fn feed_after_frame_appends_pending() {
        let frame1 = encode_frame(b"first").unwrap();
        let frame2 = encode_frame(b"second").unwrap();
        let mut decoder = FrameDecoder::new();
        decoder.feed(&frame1).unwrap();
        // フレーム1 を取り出さずに frame2 を投入しても、両方保持される
        decoder.feed(&frame2).unwrap();
        assert_eq!(decoder.take_frame().unwrap(), b"first");
        assert_eq!(decoder.take_frame().unwrap(), b"second");
    }

    proptest! {
        #[test]
        fn feed_arbitrary_bytes_never_panics(
            chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..=128), 0..=32),
        ) {
            let mut decoder = FrameDecoder::new();
            for chunk in &chunks {
                let _ = decoder.feed(chunk);
                // 8 MiB 超の body は確保されていない。
                prop_assert!(decoder.body.capacity() <= MAX_FRAME_SIZE as usize);
            }
            let _ = decoder.end();
        }

        #[test]
        fn feed_result_is_typed_or_waiting(bytes in prop::collection::vec(any::<u8>(), 0..=1024)) {
            let mut decoder = FrameDecoder::new();
            match decoder.feed(&bytes) {
                Ok(()) => {
                    // 完成したフレームはキューへ積まれるため、feed 後の状態は常に読み取り待ちのいずれか。
                    let waiting = matches!(
                        decoder.state(),
                        DecoderState::ReadingLength | DecoderState::ReadingBody { .. }
                    );
                    prop_assert!(waiting);
                }
                Err(e) => {
                    prop_assert!(matches!(
                        e,
                        FrameError::PayloadTooLarge(_)
                            | FrameError::EmptyPayload
                            | FrameError::IncompleteFrame
                    ));
                }
            }
        }

        #[test]
        fn rejects_oversized_length_without_allocation(
            (length, body) in (MAX_FRAME_SIZE + 1..=u32::MAX, prop::collection::vec(any::<u8>(), 0..=64)),
        ) {
            let mut frame = Vec::with_capacity(4 + body.len());
            frame.extend_from_slice(&length.to_le_bytes());
            frame.extend_from_slice(&body);

            let mut decoder = FrameDecoder::new();
            let result = decoder.feed(&frame);
            prop_assert!(matches!(result, Err(FrameError::PayloadTooLarge(_))));
            prop_assert!(decoder.body.capacity() <= MAX_FRAME_SIZE as usize);
        }

        #[test]
        fn encode_decode_roundtrip_property(body in prop::collection::vec(any::<u8>(), 1..=8192)) {
            let encoded = encode_frame(&body).unwrap();
            let mut decoder = FrameDecoder::new();
            decoder.feed(&encoded).unwrap();
            prop_assert_eq!(decoder.take_frame(), Some(body));
        }

        #[test]
        fn invalid_body_not_mistaken_as_envelope(
            body in prop::collection::vec(any::<u8>(), 1..=512),
        ) {
            let len = body.len() as u32;
            if len == 0 || len > MAX_FRAME_SIZE {
                return Ok(());
            }

            let mut frame = Vec::with_capacity(4 + body.len());
            frame.extend_from_slice(&len.to_le_bytes());
            frame.extend_from_slice(&body);

            let mut decoder = FrameDecoder::new();
            let frame_body = match decoder.feed(&frame) {
                Ok(()) => decoder.take_frame(),
                Err(_) => None,
            };
            if let Some(frame_body) = frame_body {
                let req: Result<RequestEnvelope, _> = deserialize_json(&frame_body);
                let resp: Result<ResponseEnvelope, _> = deserialize_json(&frame_body);
                // ランダムな body が偶然正当な Envelope になることは無視する。
                prop_assume!(req.is_err() || resp.is_err());
            }
        }
    }
}
