//! パレットの読み取り DTO。

use serde::{Deserialize, Serialize};

/// 1 つのパレットが持つ色数。
///
/// アプリケーションが固定長の配列として定めており、パレットごとに増減しない。
pub const PALETTE_COLOR_COUNT: usize = 64;

/// パレットの色 1 件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgba {
    /// 赤成分。
    pub r: u8,
    /// 緑成分。
    pub g: u8,
    /// 青成分。
    pub b: u8,
    /// 不透明度。
    ///
    /// **常に 255 である。** パレットは透明度の情報を持たない。値を落とさず
    /// 載せるのは、アプリケーションが返す形をそのまま写すためである。
    pub a: u8,
}

/// パレット 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaletteEntry {
    /// パレット名。
    pub name: String,
    /// 色。件数は常に [`PALETTE_COLOR_COUNT`] である。
    pub colors: Vec<Rgba>,
}
