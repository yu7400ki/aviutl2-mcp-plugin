//! 完了コールバックが渡す pixel buffer の検証と詰め直し。
//!
//! ここへ来る寸法は**ホストから来る信頼できない整数**である。SDK ラッパーは
//! 符号付きの寸法を検査せずに `u32` へ写すため、負値は 2^31 以上の巨大な値に
//! なる。長さは算出時に破綻すると空スライスへ縮退するため、寸法と長さが
//! 整合しない組が渡り得る。したがって全ての算術を checked で行い、規則を
//! 1 つでも破ったら詰め直しへ進まない。
//!
//! 詰め直しでは `pitch`（1 行のバイト数）が `width * 4` と一致しない場合を
//! **主経路として扱う**。`pitch` は行の詰め物を許すために存在する値であり、
//! 詰め物がある方が正常であり得る。一致する場合だけを型付きで扱う補助は、
//! 詰め物があると黙って値を返さなくなるため用いない。
//!
//! チャンネルの並べ替えは行わない。ホストが渡す画素はメモリ上で `r, g, b, a`
//! の順に並んでおり、PNG の RGBA8 も同じ並びである。詰め物を除けば足りる。

use crate::render::error::BufferRule;
use aviutl2_mcp_core::MAX_RENDER_FRAME_BYTES;

/// 1 画素のバイト数。
pub const BYTES_PER_PIXEL: u32 = 4;

/// 寸法として受け取ってよい上限。
///
/// SDK は寸法を符号付きで返す。これを超える値は負値が符号なしとして
/// 写された結果であり、buffer の長さとの整合が成り立たない。
const MAX_DIMENSION: u32 = i32::MAX as u32;

/// 検証を通った pixel buffer の配置。
///
/// 生成できるのは [`validate_layout`] だけであり、規則を 1 つでも破った組から
/// この型を作る経路は無い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameLayout {
    width: u32,
    height: u32,
    row_bytes: u32,
    pitch: u32,
}

impl FrameLayout {
    /// 画像の幅（画素）。
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 画像の高さ（画素）。
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 詰め物を除いた 1 行のバイト数。
    pub fn row_bytes(&self) -> u32 {
        self.row_bytes
    }

    /// ホストが返した 1 行のバイト数。[`FrameLayout::row_bytes`] 以上。
    pub fn pitch(&self) -> u32 {
        self.pitch
    }

    /// 詰め物を除いた画像全体のバイト数。
    pub fn pixel_bytes(&self) -> usize {
        self.row_bytes as usize * self.height as usize
    }

    /// ホストが渡す buffer が持つべきバイト数。
    pub fn buffer_bytes(&self) -> usize {
        self.pitch as usize * self.height as usize
    }
}

/// 詰め物を除いた RGBA8 画像。
#[derive(Clone, PartialEq, Eq)]
pub struct ExtractedFrame {
    /// 画像の幅（画素）。
    pub width: u32,
    /// 画像の高さ（画素）。
    pub height: u32,
    /// 長さは `width * height * 4`。
    pub pixels: Vec<u8>,
}

/// 画素を出さない表示。
///
/// 画像には利用者のプロジェクトの内容が写る。導出した表示のままにすると、
/// この型を含む値をどこかで表示に流した時点で画素列がそのまま出る。
impl std::fmt::Debug for ExtractedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtractedFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pixel_bytes", &self.pixels.len())
            .finish()
    }
}

/// 寸法と長さの整合を検証する。
///
/// 検証する規則は次の 7 つで、破れた順に最初の 1 つを返す。
///
/// 1. `frame` が要求したフレームと一致する
/// 2. `width` / `height` / `pitch` がいずれも `i32::MAX` 以下
/// 3. `width` と `height` がいずれも 0 でない
/// 4. `width * 4` が `u32` に収まる
/// 5. `pitch` が `width * 4` 以上
/// 6. `pitch * height` が `buffer_len` と一致する
/// 7. 詰め物を除いた大きさが [`MAX_RENDER_FRAME_BYTES`] 以下
///
/// 規則 6 は buffer が空スライスへ縮退した場合をここで捕まえる。これを
/// 検査しないと、0 バイトの画像が成功として通る。
///
/// 長さだけを受け取り buffer そのものを受け取らないのは、任意の組に対して
/// 確保を伴わずに可否を確かめられるようにするためである。
pub fn validate_layout(
    requested_frame: u32,
    frame: u32,
    width: u32,
    height: u32,
    pitch: u32,
    buffer_len: usize,
) -> Result<FrameLayout, BufferRule> {
    if frame != requested_frame {
        return Err(BufferRule::FrameMismatch);
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION || pitch > MAX_DIMENSION {
        return Err(BufferRule::DimensionOutOfRange);
    }
    if width == 0 || height == 0 {
        return Err(BufferRule::EmptyDimension);
    }
    let row_bytes = width
        .checked_mul(BYTES_PER_PIXEL)
        .ok_or(BufferRule::RowBytesOverflow)?;
    if pitch < row_bytes {
        return Err(BufferRule::PitchTooSmall);
    }
    let buffer_bytes = (pitch as usize)
        .checked_mul(height as usize)
        .ok_or(BufferRule::BufferLengthMismatch)?;
    if buffer_len != buffer_bytes {
        return Err(if buffer_len == 0 {
            BufferRule::EmptyBuffer
        } else {
            BufferRule::BufferLengthMismatch
        });
    }
    let pixel_bytes = (row_bytes as usize)
        .checked_mul(height as usize)
        .ok_or(BufferRule::FrameTooLarge)?;
    // 上限は `u64` で持つ。比較を `u64` の側で行えば、`usize` の幅に依らず
    // 同じ判定になる（`usize` から `u64` への変換は値を落とさない）。
    if pixel_bytes as u64 > MAX_RENDER_FRAME_BYTES {
        return Err(BufferRule::FrameTooLarge);
    }
    Ok(FrameLayout {
        width,
        height,
        row_bytes,
        pitch,
    })
}

/// 検証を通した buffer から詰め物を除いて所有メモリへ写す。
///
/// `buffer` は `layout` を得たときと同じものでなければならない。長さが違えば
/// 詰め直しへ進まず、規則 6 の破れとして返す。
pub fn de_stride(layout: &FrameLayout, buffer: &[u8]) -> Result<Vec<u8>, BufferRule> {
    if buffer.len() != layout.buffer_bytes() {
        return Err(BufferRule::BufferLengthMismatch);
    }
    let row_bytes = layout.row_bytes as usize;
    let mut pixels = Vec::with_capacity(layout.pixel_bytes());
    for row in buffer.chunks_exact(layout.pitch as usize) {
        pixels.extend_from_slice(&row[..row_bytes]);
    }
    Ok(pixels)
}

/// 寸法と長さを検証し、詰め物を除いた RGBA8 画像を返す。
pub fn extract(
    requested_frame: u32,
    frame: u32,
    width: u32,
    height: u32,
    pitch: u32,
    buffer: &[u8],
) -> Result<ExtractedFrame, BufferRule> {
    let layout = validate_layout(requested_frame, frame, width, height, pitch, buffer.len())?;
    let pixels = de_stride(&layout, buffer)?;
    Ok(ExtractedFrame {
        width: layout.width,
        height: layout.height,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// 規則を 1 つも破らない組。各テストはここから 1 か所だけを崩す。
    fn valid_call() -> (u32, u32, u32, u32, u32, usize) {
        // pitch は width * 4 と一致させる。詰め物の有無そのものを崩す規則が
        // 無いため、既定は最も素直な形にしておく。
        (7, 7, 4, 3, 16, 48)
    }

    fn validate(call: (u32, u32, u32, u32, u32, usize)) -> Result<FrameLayout, BufferRule> {
        validate_layout(call.0, call.1, call.2, call.3, call.4, call.5)
    }

    #[test]
    fn a_consistent_call_passes_every_rule() {
        let layout = validate(valid_call()).expect("整合した組が拒否されました");
        assert_eq!(layout.width(), 4);
        assert_eq!(layout.height(), 3);
        assert_eq!(layout.row_bytes(), 16);
        assert_eq!(layout.pitch(), 16);
        assert_eq!(layout.pixel_bytes(), 48);
    }

    #[test]
    fn rule_1_rejects_a_frame_that_is_not_the_requested_one() {
        let mut call = valid_call();
        call.1 = 8;
        assert_eq!(validate(call), Err(BufferRule::FrameMismatch));
    }

    #[test]
    fn rule_2_rejects_dimensions_beyond_the_signed_range() {
        // 負値が符号なしとして写ると 2^31 以上になる。3 つの寸法それぞれで
        // 個別に確かめる。長さは崩した寸法に合わせても届かないため、規則 2 が
        // 先に拒否することを見ている。
        for (width, height, pitch) in [
            (MAX_DIMENSION + 1, 3, 16),
            (4, MAX_DIMENSION + 1, 16),
            (4, 3, MAX_DIMENSION + 1),
        ] {
            assert_eq!(
                validate_layout(7, 7, width, height, pitch, 48),
                Err(BufferRule::DimensionOutOfRange),
                "({width}, {height}, {pitch})"
            );
        }
    }

    #[test]
    fn rule_3_rejects_an_empty_dimension() {
        assert_eq!(
            validate_layout(7, 7, 0, 3, 16, 0),
            Err(BufferRule::EmptyDimension)
        );
        assert_eq!(
            validate_layout(7, 7, 4, 0, 16, 0),
            Err(BufferRule::EmptyDimension)
        );
    }

    #[test]
    fn rule_4_rejects_a_width_whose_row_does_not_fit_in_u32() {
        // 規則 2 を通る最大の幅でも、1 行のバイト数は u32 に収まらない。
        let width = MAX_DIMENSION;
        assert_eq!(
            validate_layout(7, 7, width, 1, MAX_DIMENSION, MAX_DIMENSION as usize),
            Err(BufferRule::RowBytesOverflow)
        );
    }

    #[test]
    fn rule_5_rejects_a_pitch_shorter_than_a_row() {
        let mut call = valid_call();
        call.4 = 15;
        call.5 = 45;
        assert_eq!(validate(call), Err(BufferRule::PitchTooSmall));
    }

    #[test]
    fn rule_6_rejects_a_length_that_does_not_match_the_dimensions() {
        let mut call = valid_call();
        call.5 = 47;
        assert_eq!(validate(call), Err(BufferRule::BufferLengthMismatch));
    }

    #[test]
    fn rule_6_rejects_a_buffer_that_collapsed_to_empty() {
        // ラッパーは長さの算出が破綻すると空スライスへ倒す。検査しなければ
        // 0 バイトの画像が成功として通る。
        let mut call = valid_call();
        call.5 = 0;
        assert_eq!(validate(call), Err(BufferRule::EmptyBuffer));
    }

    #[test]
    fn rule_7_rejects_a_frame_larger_than_the_cap() {
        // 詰め物を除いた大きさが上限をちょうど 1 バイト超える組を作る。
        let width = 4 * 1024;
        let row_bytes = width * 4;
        let height = (MAX_RENDER_FRAME_BYTES / row_bytes as u64) as u32 + 1;
        let buffer_len = row_bytes as usize * height as usize;
        assert!(buffer_len as u64 > MAX_RENDER_FRAME_BYTES);
        assert_eq!(
            validate_layout(7, 7, width, height, row_bytes, buffer_len),
            Err(BufferRule::FrameTooLarge)
        );
    }

    #[test]
    fn rule_7_accepts_a_frame_exactly_at_the_cap() {
        let width = 4 * 1024;
        let row_bytes = width * 4;
        let height = (MAX_RENDER_FRAME_BYTES / row_bytes as u64) as u32;
        let buffer_len = row_bytes as usize * height as usize;
        assert_eq!(buffer_len as u64, MAX_RENDER_FRAME_BYTES);
        assert!(validate_layout(7, 7, width, height, row_bytes, buffer_len).is_ok());
    }

    #[test]
    fn padding_is_removed_row_by_row() {
        // 幅 2 画素・高さ 3 行、1 行あたり 4 バイトの詰め物を置く。詰め物には
        // 画素と紛れない値を入れ、出力へ 1 バイトも混ざらないことを見る。
        const PAD: u8 = 0xEE;
        let width = 2u32;
        let height = 3u32;
        let row_bytes = width * 4;
        let pitch = row_bytes + 4;
        let mut buffer = Vec::new();
        let mut expected = Vec::new();
        for row in 0..height as u8 {
            for column in 0..width as u8 {
                let pixel = [row * 16 + column, row, column, 0xFF];
                buffer.extend_from_slice(&pixel);
                expected.extend_from_slice(&pixel);
            }
            buffer.extend_from_slice(&[PAD; 4]);
        }

        let frame = extract(7, 7, width, height, pitch, &buffer).expect("詰め物のある入力");
        assert_eq!(frame.width, width);
        assert_eq!(frame.height, height);
        assert_eq!(frame.pixels, expected);
        assert!(
            !frame.pixels.contains(&PAD),
            "詰め物が出力へ混ざりました: {:?}",
            frame.pixels
        );
    }

    #[test]
    fn a_buffer_without_padding_is_copied_as_is() {
        let buffer: Vec<u8> = (0..48u8).collect();
        let frame = extract(7, 7, 4, 3, 16, &buffer).expect("詰め物のない入力");
        assert_eq!(frame.pixels, buffer);
    }

    #[test]
    fn de_stride_refuses_a_buffer_of_a_different_length() {
        let layout = validate(valid_call()).unwrap();
        assert_eq!(
            de_stride(&layout, &[0u8; 47]),
            Err(BufferRule::BufferLengthMismatch)
        );
    }

    /// 境界へ寄せた寸法の生成器。
    ///
    /// 一様乱数は規則 4〜7 の境界にほとんど当たらない。`u32::MAX` 近傍・0・
    /// 符号の境界・小さな値を混ぜ、境界そのものを踏ませる。
    fn dimension() -> impl Strategy<Value = u32> {
        prop_oneof![
            Just(0u32),
            1u32..=64,
            Just(MAX_DIMENSION - 1),
            Just(MAX_DIMENSION),
            Just(MAX_DIMENSION + 1),
            Just(u32::MAX - 1),
            Just(u32::MAX),
            any::<u32>(),
        ]
    }

    /// 幅から `width * 4` の周辺へ寄せた pitch の生成器。
    fn pitch_for(width: u32) -> impl Strategy<Value = u32> {
        let row_bytes = width.saturating_mul(BYTES_PER_PIXEL);
        prop_oneof![
            Just(row_bytes.saturating_sub(1)),
            Just(row_bytes),
            Just(row_bytes.saturating_add(1)),
            dimension(),
        ]
    }

    /// 整合する長さの周辺へ寄せた buffer 長の生成器。
    fn buffer_len_for(pitch: u32, height: u32) -> impl Strategy<Value = usize> {
        let exact = (pitch as usize).saturating_mul(height as usize);
        prop_oneof![
            Just(0usize),
            Just(exact.saturating_sub(1)),
            Just(exact),
            Just(exact.saturating_add(1)),
            0usize..=4096,
        ]
    }

    proptest! {
        /// 任意の組に対して panic せず、overflow せず、必ず可否を返す。
        ///
        /// 入力はホストから来る信頼できない整数の組であり、規則を 1 つ落とすと
        /// 範囲外参照か過大確保になる。可否そのものより「必ず答えを返す」ことを
        /// 固定する。
        #[test]
        fn validation_always_answers(
            (requested_frame, frame) in (any::<u32>(), any::<u32>()),
            (width, height) in (dimension(), dimension()),
        ) {
            let pitch = width.saturating_mul(BYTES_PER_PIXEL);
            let buffer_len = (pitch as usize).saturating_mul(height as usize);
            let _ = validate_layout(requested_frame, frame, width, height, pitch, buffer_len);
        }

        /// 規則 5〜7 の境界へ寄せた組でも panic せず overflow しない。
        #[test]
        fn validation_never_overflows_on_boundary_shapes(
            (width, height, pitch, buffer_len) in (dimension(), dimension())
                .prop_flat_map(|(width, height)| {
                    (Just(width), Just(height), pitch_for(width))
                })
                .prop_flat_map(|(width, height, pitch)| {
                    (
                        Just(width),
                        Just(height),
                        Just(pitch),
                        buffer_len_for(pitch, height),
                    )
                }),
        ) {
            let _ = validate_layout(0, 0, width, height, pitch, buffer_len);
        }

        /// 検証を通った組は、詰め直しても必ず `width * height * 4` になる。
        ///
        /// 確保が過大にならないことを兼ねて、上限より十分小さい寸法だけを流す。
        #[test]
        fn accepted_shapes_copy_into_exactly_the_expected_size(
            width in 1u32..=64,
            height in 1u32..=64,
            padding in 0u32..=64,
        ) {
            let pitch = width * BYTES_PER_PIXEL + padding;
            let buffer = vec![0u8; pitch as usize * height as usize];
            let frame = extract(0, 0, width, height, pitch, &buffer)
                .expect("整合した組が拒否されました");
            prop_assert_eq!(
                frame.pixels.len(),
                width as usize * height as usize * BYTES_PER_PIXEL as usize
            );
        }

        /// 検証を通らなかった組では詰め直しへ進まない。
        #[test]
        fn rejected_shapes_never_allocate(
            width in dimension(),
            height in dimension(),
            pitch in dimension(),
        ) {
            // buffer は常に空を渡す。長さが整合する組はほぼ現れないため、
            // ここを通る大半は拒否される。確保を伴う経路（`extract`）が
            // 拒否された組で呼ばれないことを、戻り値の型で確かめる。
            let buffer: &[u8] = &[];
            match validate_layout(0, 0, width, height, pitch, buffer.len()) {
                Ok(layout) => {
                    prop_assert_eq!(layout.buffer_bytes(), 0);
                }
                Err(_) => {
                    prop_assert!(extract(0, 0, width, height, pitch, buffer).is_err());
                }
            }
        }
    }
}
