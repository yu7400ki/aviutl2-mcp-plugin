# AviUtl2 MCP Plugin

MCP クライアントから AviUtl2 のプロジェクトを参照し、編集するためのローカル連携プラグイン。

```
MCP クライアント ──stdio──▶ aviutl2-mcp-server.exe ──名前付きパイプ──▶ aviutl2-mcp-plugin.aux2
                                                                        （AviUtl2 のプロセス内）
```

- 同時に起動している複数の AviUtl2 を扱う。どのインスタンスへ宛てるかは要求ごとに `instance_id` で指す
- 1 つの要求が進めるプロジェクトの版は高々 1 である。`apply_batch` で複数の操作を束ねれば、Undo 1 回で戻せる

## 動作条件

- Windows
- AviUtl2 v2.1.4 以上

## インストール

1. [Releases](https://github.com/yu7400ki/aviutl2-mcp-plugin/releases) から `aviutl2-mcp-plugin-vX.Y.Z.au2pkg.zip` を入手する
2. AviUtl2 のプレビュー画面へドラッグ＆ドロップする
3. AviUtl2 を再起動する

AviUtl2 のアプリケーションデータフォルダの `Plugin` へ、次の 2 つが置かれる。
このフォルダは通常 `C:\ProgramData\aviutl2` である。

| ファイル | 役割 |
|---|---|
| `aviutl2-mcp-plugin.aux2` | AviUtl2 内で動く汎用プラグイン |
| `aviutl2-mcp-server.exe` | MCP クライアントが起動するサーバー |

## MCP クライアントへの接続

### エージェントプラグインを生成させる

AviUtl2 の「設定」メニューから「AviUtl2 MCP」を開き、「エージェントプラグイン」ページで「エージェントプラグインを生成する」を入れる。
`%LOCALAPPDATA%\AviUtl2Mcp` の下へ、marketplace と plugin manifest、MCP の接続設定、skill、サーバー実行体の複製が書き出される。
Claude Code と [agent-plugins.org](https://agent-plugins.org) のどちらを書くかは個別に選べる。

Claude Code なら、`%LOCALAPPDATA%\AviUtl2Mcp` を marketplace として追加し、`aviutl2` プラグインを入れる。
接続設定と skill が同時に入る。

同梱する skill は `aviutl2-editing` の 1 つで、座標系、レイヤー、オブジェクトとエフェクト、オブジェクトエイリアスの綴りを扱う。

### 接続設定を手で書く

skill を伴わずに繋ぐ場合は、クライアントの MCP 設定へ次を書く。

```json
{
  "mcpServers": {
    "aviutl2": {
      "type": "stdio",
      "command": "C:\\ProgramData\\aviutl2\\Plugin\\aviutl2-mcp-server.exe"
    }
  }
}
```

`command` はインストール先の実際のパスに合わせる。

## 公開している tool

`list_instances` でインスタンスを列挙し、その `instance_id` を以降のすべての要求に添える。

| 族 | tool |
|---|---|
| 発見 | `list_instances` |
| 読み取り | `get_edit_info` `get_current_scene` `list_layers` `list_objects` `get_object` `get_selection` `describe_effects` `get_effect_item_values` `list_available_effects` `list_object_aliases` `list_fonts` `list_palettes` `list_modules` |
| 編集 | `create_object` `move_object` `set_object_item` `set_object_name` `delete_object` `create_object_section` `move_object_section` `delete_object_section` `add_effect` `move_effect` `set_effect_enabled` `delete_effect` `set_layer_state` `set_scene_settings` `set_selection` `set_grid_bpm` `apply_batch` |
| 描画 | `render_frame` |

## 設定

- **公開する tool**：`list_instances` を除く 31 個を個別に切り替える
- **動作**：ログ、待ち時間、保存と掃除の 3 群。ログの水準、要求に与える時間の予算の倍率、描画成果物の寿命と上限を持つ
- **エージェントプラグイン**：生成の可否、クライアントの選択、skill の同梱

設定は `%LOCALAPPDATA%\AviUtl2Mcp\settings.json` に置かれる。

## できないこと

- **プロジェクトファイルの保存と読み込み。** SDK に保存させる関数が無く、あるのは利用者が保存したときに呼ばれるコールバックだけである
- **現在シーン以外の参照とシーンの切り替え。** SDK に全シーンを列挙する API も切り替える API も無いため、操作は現在シーンに閉じる
- **TCP や HTTP を使ったリモート操作**

## ライセンス

MIT
