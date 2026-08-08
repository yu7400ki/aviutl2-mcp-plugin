// 設定項目の面——選択肢の候補と、数値の値域および小数桁——を、起動中の
// AviUtl2 への書き込み検証で起こす。
//
// **SDK に供給源が無い。** 設定項目の列挙がコールバックへ渡すのは名前と種別だけ
// であり、選択肢を返す関数も値域を返す関数もヘッダーに存在しない。どちらの面も
// ホストは持っているが我々へ渡す口が無く、**同じ問題に 2 つ目の答えを作らない**
// ——書き込みの成否と、ホストが倒した値そのものを判定に使う。
//
// # 候補
//
// 候補の在庫は言語ファイルの `[Effect]` 節から得る。ただし**どの設定項目の選択肢
// なのかはファイルに書かれていない。** 効果名・項目名・選択肢ラベルが重複排除
// されたまま平坦に並ぶだけである。所属は、スクラッチのオブジェクトへ実際に
// 書き込んで受理されるかどうかで決める。**選択肢に無い値の書き込みは状態を
// 変えない**ため、受理された値だけが残り、スクラッチのオブジェクトを最後に
// 消せば副作用は残らない。
//
// 対象は、**書き込みが選択肢の形（`choice`）を受け取る 4 種**——`select`
// `combo` `mask` `figure`——である。この 4 種は書かれた値を候補の集合に対して
// 解決する種別であり、解決できなければ拒む。**完全一致で照合される種別とは
// 別の切り口である**——`text` も完全一致で照合されるが、任意の文字列を受け
// 取るため所属を決められない。`combo` はリストと文字の複合だが任意の文字列を
// 受け取るわけではなく、`図形 / 図形の種類` へ実在しない図形を書けば失敗し、
// `四角形` は受理される。
//
// **在庫が探索の範囲を絞る。** 組み込みの図形（`円` `四角形` `三角形` `五角形`
// `六角形` `星型` `ハート`）は訳語を持つため在庫に在り、データディレクトリの
// svg はそもそも訳す対象ではないため在庫に無い。**表へ入るのは在庫のラベル
// だけである**——試すのが在庫のラベルだけであることに加え、走査を始めた時点の
// 値も在庫に無ければ落とす。`縁取り / パターン画像` の既定値は空文字列であり、
// これが基底へ混じれば表は環境ごとに違うものになる。ファイル由来の候補は表では
// なく、項目の説明が述べる（`図形の種類` は「ボタンクリックでsvgファイルを選択
// 出来ます」を返す）。
//
// **判定が効くことを項目ごとに確かめる。** 解決できない値を拒むことを実測した
// のは `combo` 2 件だけであり、4 種の全項目が同じ振る舞いをするとは測っていない。
// そこで在庫を総当たりする前に、**候補になり得ない文字列を 1 回書く**
// （`NEGATIVE_CONTROL`）。拒まれればその項目は値を集合に対して解決しており、
// 受理されれば解決していない——後者は総当たりしても受理の記録が並ぶだけである
// ため、在庫を試さずに報告へ回す。
//
// # 値域
//
// 書き戻し照合がそのまま測定器になる。極端に大きい値・小さい値・小数を多く持つ
// 値を書くと、書き込みは `item_value_not_applied` で失敗し、**ホストが倒した値が
// 応答の `observed_value` に載る。** それが上限・下限・小数桁である。
//
// **探りの値が値域の内側へ収まった側は測れない。** 受理されたことが言うのは
// 「端が探りの外にある」ことだけであり、端が無いことの証明にはならない。
// 測れなかった側は記録しない——**表に載せるのは測れた側だけとする。**
//
// 小数桁は**3 回の探りが返した表記を突き合わせて決める。** ホストがクランプした
// 値と丸めた値を同じ桁で書き出すかは測っていないため、1 つの表記だけを根拠に
// すると、クランプの表記が桁を落としている場合に誤った小数桁が黙って表へ入る。
// 桁が食い違ったら、その項目の小数桁は測れていないものとして扱う。
//
// **`item_value_not_applied` 以外の失敗は測定ではない。** 移動を持つ項目は
// `track_movement_present` で失敗し、探りの値そのものをホストが解釈できなければ
// また別の理由で失敗する。いずれもその項目を測らずに報告へ回す——黙って値域を
// 持たない項目にはしない。
//
// # 2 段で埋める理由
//
// 在庫が重複排除されているため、境界拡張だけでは取りこぼす。同じ項目の選択肢は
// 在庫の中で連続するが、他の項目と共有するラベルはその連なりから抜けている。
//
// 1. 境界拡張 — 各項目の現在値を在庫の中に見つけ、そこから上下へ 1 件ずつ試す
// 2. 未割当の回収 — 1 段目で見つからなかったラベルを、項目ごとに残らず試す
//
// **2 段目の候補を「どの項目にも割り当たらなかったラベル」へ絞らない。** 絞ると
// 項目をまたいで共有されるラベルが落ちる——`閃光 / 合成モード` の `光成分のみ` は
// 他の項目が先に主張するため候補から消え、`ディスプレイスメントマップ /
// 変形方法` の `拡大変形` と `回転変形` も同様に落ちた。書き込みは毎秒 300 回を
// 超えるため、在庫を項目ごとに全件試しても実時間は分単位に収まる。
//
// # スクラッチのオブジェクトは同時に 1 つだけ生かす
//
// **timeline に乗っているオブジェクトの数が、1 編集あたりの費用を決める。**
// ホストは編集のたびにプレビューを描き直し、そのフレームに乗るオブジェクトを
// 全て通す。走査に要るオブジェクトを作り置きして最後まで残すと、1 編集あたりの
// 描画時間が対象の数に比例して伸びる——モーションブラーのような重い効果が
// 混じれば数ミリ秒から数秒の桁へ移る。
//
// **これは走査の速さの問題では済まない。** 描画はホストのメインスレッドを塞ぐ
// ため、その間 plugin は生存確認の ping を返せず、server はインスタンスを
// `instance_stale` として扱う。作り置きの形は、対象が増えるほど確実に落ちる。
//
// そこで走査を**オブジェクトの作り方で束ね**、束ごとに「作る → その上に乗る
// (効果, 項目) を全て測る → 消す」を回す（[`withScratchObject`]）。同時に生きる
// スクラッチは常に 1 つであり、**1 編集あたりの描画費用は走査の総量に依らない。**
// 片付けも同じ形から出る——途中で落ちても timeline に残るのは高々 1 つである。

import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { Mcp, REPOSITORY } from "./mcp.mjs";

/** 生成物の既定の書き出し先。 */
const DEFAULT_OUTPUT = join(REPOSITORY, "crates", "plugin", "data", "effect_item_facets.json");

/**
 * 言語ファイルを探す場所。先に見つかった方を使う。
 *
 * 開発用ディレクトリを先に見る。**そこで動いている AviUtl2 へ書き込むのだから、
 * 在庫もそのインスタンスが読んでいる言語ファイルから取る。**
 */
const LANGUAGE_DIRECTORIES = [
  join(REPOSITORY, ".aviutl2-cli", "development", "data", "Language"),
  join(process.env.ProgramData ?? "C:\\ProgramData", "aviutl2", "Language"),
];

/**
 * 書き込みの総回数の上限。暴走したときにここで止まる。
 *
 * 見積もりは 2 つの面の和である。
 *
 * - 候補: 1 項目あたり、負の対照が 1 回、境界拡張が最悪
 *   `MAX_STEPS_PER_DIRECTION` × 2 方向、2 段目が在庫の全件でおよそ 600 回。
 *   合わせて 1001 回。対象が 4 種になった後の項目を 60 組と見て **60,060 回**。
 *   **判定が効かない項目は負の対照の 1 回だけで終わる**——在庫を総当たりしない
 * - 値域: 1 項目あたり上限・下限・小数桁で 3 回。数値の項目を 1000 組と見て
 *   **3,000 回**
 *
 * **上限は見積もりの倍を超える位置に置く。** 予算は暴走を止める柵であり、
 * 正常な走査が触れる値ではない。
 */
const DEFAULT_MAX_WRITES = 130000;

/** 境界拡張で 1 方向へ進む最大の歩数。 */
const MAX_STEPS_PER_DIRECTION = 200;

/** describe_effects が 1 度に受け取れる効果の数。 */
const DESCRIBE_BATCH = 10;

/** 走査する効果の種別。 */
const EFFECT_TYPES = ["input", "output", "control", "filter", "transition"];

/**
 * 候補を集める設定項目の種別。
 *
 * **書き込みが選択肢の形を受け取る 4 種である。** 値を候補の集合に対して解決
 * する種別がこれだけであり、解決する以上は解決できない値を拒める。完全一致で
 * 照合される種別はこれより広く、そちらには任意の文字列を受け取る `text` などが
 * 含まれる。
 */
const CHOICE_ITEM_TYPES = ["select", "combo", "mask", "figure"];

/** 値域を測る設定項目の種別。 */
const RANGE_ITEM_TYPES = ["integer", "number"];

/** 小数桁を測る設定項目の種別。**整数の項目へは小数を持つ値を書く形が無い。** */
const DECIMALS_ITEM_TYPES = ["number"];

/** 設定項目の種別から、その項目について測る面を引く。 */
const FACET_OF_ITEM_TYPE = new Map([
  ...CHOICE_ITEM_TYPES.map((type) => [type, "choices"]),
  ...RANGE_ITEM_TYPES.map((type) => [type, "range"]),
]);

/**
 * 値域を測る探りの値。
 *
 * **`integer` と `number` で別に決めた上で、同じ大きさに落ち着いている。**
 * `integer` はホスト側の整数の幅に縛られる——SDK のヘッダーは幅を述べておらず、
 * `i64` の両端を書くと収まらない可能性がある。10 億は 32 ビット整数の上限の
 * 半分以下であり、設定項目が取り得る大きさ（`サイズ` の上限が 4000、座標が
 * 数万）より 5 桁大きい。`number` は倍精度であるためこの縛りを受けないが、
 * **極端な値をホストが解釈できるかは測っていない。まず収まる範囲で試す。**
 */
const RANGE_PROBE = { max: 1_000_000_000, min: -1_000_000_000 };

/**
 * 小数桁を測る探りの値。
 *
 * **値域の内側に在る必要は無いが、内側に在るかで表記の根拠が変わる。** 内側なら
 * ホストは項目の桁へ丸めた値を返し、その表記が小数桁である。外側なら端へ倒され
 * るが、**倒した値をホストが何桁で書き出すかは測っていない。** 桁を落として
 * 書き出すのなら、この探りだけを根拠にすると誤った小数桁が表へ入る。
 * したがって `measureRange` は 3 回の探りが返した表記を突き合わせ、食い違えば
 * 小数桁を測れていないものとして扱う。
 *
 * 受理されたときも測れない。そのとき分かるのは 9 桁がそのまま残ったことだけで
 * ある。
 */
const DECIMALS_PROBE = 0.123456789;

/**
 * 判定が効くことを項目ごとに確かめる負の対照。
 *
 * **在庫のラベルには成り得ない。** 在庫は `名前=訳語` の行から `=` の手前を
 * 取るため（`readInventory`）、`=` を含む文字列が在庫に現れることは無い。
 *
 * これが拒まれた項目は、書かれた値を候補の集合に対して解決している——**受理
 * された値が候補である**と言える。受理された項目は解決していないため、在庫を
 * 総当たりしても受理の記録が並ぶだけであり、試さずに報告へ回す。
 */
const NEGATIVE_CONTROL = "aviutl2-mcp=候補ではない文字列";

/** 書き込みが受理されなかったことを表す応答の理由。 */
const NOT_APPLIED = "item_value_not_applied";

/** 書き込み 1 回の結末。 */
export const WRITE = {
  /** ホストが要求どおりの値を持った。 */
  accepted: "accepted",
  /** ホストが値を倒したか捨てた。読み直した値が `observed` に入る。 */
  notApplied: "not_applied",
  /** 予算を使い切り、書き込みを行わなかった。 */
  exhausted: "exhausted",
  /** 上記以外の失敗。ホストが名乗った理由が `reason` に入る。 */
  failed: "failed",
};

/** 引数を解釈する。 */
function parseArguments(argv) {
  const options = { language: null, output: DEFAULT_OUTPUT, server: null, maxWrites: DEFAULT_MAX_WRITES };
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    const value = argv[index + 1];
    if (value === undefined) throw new Error(`${key} に値がありません`);
    if (key === "--language") options.language = value;
    else if (key === "--out") options.output = value;
    else if (key === "--server") options.server = value;
    else if (key === "--max-writes") options.maxWrites = Number.parseInt(value, 10);
    else throw new Error(`知らない引数です: ${key}`);
  }
  if (!Number.isInteger(options.maxWrites) || options.maxWrites <= 0) {
    throw new Error("--max-writes には正の整数を指定してください");
  }
  return options;
}

/**
 * 言語ファイルの `[Effect]` 節から候補の在庫を読む。
 *
 * 並びはファイルの順そのものである。**この順がホストの選択肢の順であり、
 * 並べ替えると意味を失う。**
 */
function readInventory(path) {
  const labels = [];
  const seen = new Set();
  let inEffectSection = false;
  for (const raw of readFileSync(path, "utf8").split(/\r?\n/)) {
    const line = raw.trim();
    if (line.startsWith("[")) {
      inEffectSection = line === "[Effect]";
      continue;
    }
    if (!inEffectSection || !line || line.startsWith(";")) continue;
    const separator = line.indexOf("=");
    if (separator <= 0) continue;
    const label = line.slice(0, separator);
    if (seen.has(label)) continue;
    seen.add(label);
    labels.push(label);
  }
  return labels;
}

/**
 * 在庫を持つ言語ファイルを決める。
 *
 * 明示された 1 件を使い、無ければ既定の置き場所から `[Effect]` 節を持つ最初の
 * ファイルを採る。**複数を混ぜない**——同じ項目の選択肢が在庫の中で連続する
 * 性質は 1 ファイルの中でしか成り立たない。
 */
function resolveLanguageFile(explicit) {
  if (explicit) return { path: explicit, inventory: readInventory(explicit) };
  for (const directory of LANGUAGE_DIRECTORIES) {
    let names;
    try {
      names = readdirSync(directory);
    } catch {
      continue;
    }
    for (const name of names.filter((entry) => entry.toLowerCase().endsWith(".aul2")).sort()) {
      const path = join(directory, name);
      const inventory = readInventory(path);
      if (inventory.length > 0) return { path, inventory };
    }
  }
  throw new Error(
    `${LANGUAGE_DIRECTORIES.join(" と ")} に [Effect] 節を持つ言語ファイルがありません。--language でファイルを指定してください`,
  );
}

/** 書き込み回数の予算。使い切ったら以降の書き込みを行わない。 */
class Budget {
  constructor(limit) {
    this.limit = limit;
    this.used = 0;
  }

  get exhausted() {
    return this.used >= this.limit;
  }

  take() {
    if (this.exhausted) return false;
    this.used += 1;
    return true;
  }
}

/** 起動中のインスタンスに対する 1 回の走査。 */
class Survey {
  constructor(mcp, instanceId, sceneId, epoch) {
    this.mcp = mcp;
    this.instanceId = instanceId;
    this.sceneId = sceneId;
    this.epoch = epoch;
    /** この走査が作ったオブジェクトの selector。片付けの対象はこれだけである。 */
    this.created = [];
  }

  static async open(mcp) {
    await mcp.init();
    const instances = expect(await mcp.call("list_instances", {}), "インスタンスの一覧").instances;
    if (instances.length === 0) throw new Error("AviUtl2 が起動していません");
    const instance = instances[0];
    const scene = expect(
      await mcp.call("get_current_scene", { instance_id: instance.instance_id }),
      "現在のシーン",
    ).scene;
    return new Survey(mcp, instance.instance_id, scene.id, instance.project.epoch);
  }

  call(name, args) {
    return this.mcp.call(name, { instance_id: this.instanceId, ...args });
  }

  /** 現在のシーンにあるオブジェクトを全件返す。 */
  async listObjects() {
    const items = [];
    let offset = 0;
    for (;;) {
      const page = expect(
        await this.call("list_objects", { expected_scene_id: this.sceneId, offset, limit: 200 }),
        "オブジェクトの一覧",
      );
      items.push(...page.items);
      if (!page.page.has_more) return items;
      offset = page.page.next_offset;
    }
  }

  /** 効果からオブジェクトを 1 つ作る。作れなければ `null` を返す。 */
  async createObject(effectName, layer) {
    const response = await this.call("create_object", {
      source: { type: "effect", name: effectName },
      placement: { scene_id: this.sceneId, layer, frame: 0 },
      expected_project_epoch: this.epoch,
    });
    if (response.isError) return null;
    const selector = response.data.object.selector;
    this.epoch = selector.project_epoch ?? this.epoch;
    this.created.push(selector);
    return selector;
  }

  /** selector を追従しながらオブジェクトを読む。 */
  async getObject(selector) {
    let current = selector;
    for (let attempt = 0; attempt < 4; attempt += 1) {
      const response = await this.call("get_object", { selector: current });
      if (!response.isError) return response.data;
      const fresh = response.data?.details?.current_object;
      if (!fresh) throw new Error(`オブジェクトを読めません: ${response.text}`);
      current = fresh.selector ?? fresh;
    }
    throw new Error("オブジェクトの selector に追従できません");
  }

  /**
   * 自分が作ったオブジェクトを 1 つ消す。消せたら `true` を返す。
   *
   * 読めないオブジェクトは既に消えているものとして扱い、追跡から外す。
   * **消せなかったオブジェクトのレイヤーは占有されたままである**ため、
   * 戻り値はレイヤーを再び貸し出してよいかの判断にも使う。
   */
  async destroyObject(selector) {
    let removed = false;
    try {
      const object = await this.getObject(selector);
      const response = await this.call("delete_object", { selector: object.summary.selector });
      removed = !response.isError;
    } catch {
      // 既に消えているオブジェクトは片付ける対象ではない。
    }
    this.created = this.created.filter((entry) => entry !== selector);
    return removed;
  }

  /** 自分が作ったオブジェクトを全て消す。1 件の失敗で他を諦めない。 */
  async cleanup() {
    let removed = 0;
    for (const selector of [...this.created]) {
      if (await this.destroyObject(selector)) removed += 1;
    }
    this.created = [];
    return removed;
  }
}

/**
 * スクラッチを置くレイヤーを貸し出す。
 *
 * 消せたレイヤーは次の貸し出しへ回す。**同時に生きるスクラッチが 1 つである
 * 限り、走査の規模に依らず占有するレイヤーも 1 つである。** 消せなかった
 * オブジェクトのレイヤーは返却されず、次のスクラッチはその先へ置かれる。
 */
class ScratchLayers {
  constructor(first) {
    this.next = first;
    this.free = [];
  }

  take() {
    return this.free.pop() ?? this.next++;
  }

  release(layer) {
    this.free.push(layer);
  }
}

/**
 * スクラッチのオブジェクトを 1 つ作り、渡した処理へ預けて、必ず消す。
 *
 * **同時に timeline へ乗るスクラッチを 1 つに抑えるのはこの形である。** 処理が
 * 例外で抜けても消すため、走査が途中で落ちてもプロジェクトへ残るのは高々
 * 1 つになる。
 *
 * オブジェクトを作れなかったときは処理を呼ばず、`created` が `false` の結果を
 * 返す——作れないことは呼び出し側が報告へ回す観測である。
 */
async function withScratchObject(survey, layers, effectName, use) {
  const layer = layers.take();
  const selector = await survey.createObject(effectName, layer);
  if (!selector) {
    layers.release(layer);
    return { created: false, value: null };
  }
  try {
    return { created: true, value: await use(selector) };
  } finally {
    if (await survey.destroyObject(selector)) layers.release(layer);
  }
}

/** tool の応答から構造化出力を取り出す。失敗は例外にする。 */
function expect(response, what) {
  if (response.isError) throw new Error(`${what}を取得できません: ${response.text}`);
  return response.data;
}

/** 全種別の効果を列挙し、設定項目の顔ぶれまで取る。 */
async function describeAllEffects(survey) {
  const effects = [];
  for (const type of EFFECT_TYPES) {
    let offset = 0;
    for (;;) {
      const page = expect(
        await survey.call("list_available_effects", { effect_type: type, offset, limit: 200 }),
        `${type} の効果の一覧`,
      );
      for (const item of page.items) effects.push({ name: item.name, type, items: [] });
      if (!page.page.has_more) break;
      offset = page.page.next_offset;
    }
  }
  const byName = new Map(effects.map((effect) => [effect.name, effect]));
  for (let index = 0; index < effects.length; index += DESCRIBE_BATCH) {
    const names = effects.slice(index, index + DESCRIBE_BATCH).map((effect) => effect.name);
    const described = expect(await survey.call("describe_effects", { effect_names: names }), "効果の中身");
    for (const effect of described.effects) {
      byName.get(effect.name).items = effect.items.map((item) => ({ name: item.name, itemType: item.item_type }));
    }
  }
  return effects;
}

/**
 * 対象の効果へ到達するオブジェクトの作り方を決める。
 *
 * 効果の種別ごとに経路を決め打ちしない。**効果からオブジェクトを作れるかは
 * 実際に作って確かめ**、作れない効果については、他の効果から作ったオブジェクトに
 * 最初から付いてくるかどうかを見る（`テキスト` のオブジェクトには `標準描画` が
 * 付いてくる）。
 *
 * 返すのは 効果名 → その効果を載せたオブジェクトを作れる効果名 の対応である。
 */
async function resolveHosts(survey, targetNames, effects, layers) {
  const hosts = new Map();
  const tried = new Set();

  const probe = async (sourceName) => {
    if (tried.has(sourceName)) return;
    tried.add(sourceName);
    await withScratchObject(survey, layers, sourceName, async (selector) => {
      const object = await survey.getObject(selector);
      for (const effect of object.effects) {
        if (!hosts.has(effect.name)) hosts.set(effect.name, sourceName);
      }
    });
  };

  for (const name of targetNames) {
    if (hosts.has(name)) continue;
    await probe(name);
  }
  const carriers = effects.filter((effect) => effect.type === "input").map((effect) => effect.name);
  for (const name of targetNames) {
    if (hosts.has(name)) continue;
    for (const carrier of carriers) {
      await probe(carrier);
      if (hosts.has(name)) break;
    }
  }
  return hosts;
}

/** オブジェクトの中の効果 1 件を指す状態。書き込みのたびに selector を差し替える。 */
class Target {
  constructor(survey, objectSelector, effectName, itemName) {
    this.survey = survey;
    this.objectSelector = objectSelector;
    this.effectName = effectName;
    this.itemName = itemName;
    this.effectSelector = null;
    this.currentValue = null;
  }

  /** オブジェクトを読み直し、対象の効果と現在値を取り直す。 */
  async refresh() {
    const object = await this.survey.getObject(this.objectSelector);
    this.objectSelector = object.summary.selector;
    const effect = object.effects.find((entry) => entry.name === this.effectName);
    if (!effect) throw new Error(`${this.effectName} がオブジェクトから消えています`);
    this.effectSelector = effect.selector;
    const item = effect.items.find((entry) => entry.name === this.itemName);
    if (!item) throw new Error(`${this.effectName} に ${this.itemName} がありません`);
    this.currentValue = item.value.value;
  }

  /**
   * 値を 1 つ書き込み、結末を返す。
   *
   * **拒否は状態を変えず、倒しは書き込み前の値へ巻き戻される**ため、selector は
   * そのまま次の試行に使える。受理されたときだけ応答が返す新しい selector へ
   * 差し替える。
   */
  async write(payload, budget) {
    if (!budget.take()) return { outcome: WRITE.exhausted };
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const response = await this.survey.call("set_object_item", {
        selector: this.effectSelector,
        item: this.itemName,
        value: payload,
      });
      if (!response.isError) {
        this.effectSelector = response.data.effect.selector;
        this.objectSelector = response.data.effect.selector.object;
        this.currentValue = payload.value;
        return { outcome: WRITE.accepted };
      }
      const details = response.data?.details ?? {};
      if (details.reason === NOT_APPLIED) {
        return { outcome: WRITE.notApplied, observed: details.observed_value ?? null };
      }
      if (response.data?.code !== "precondition_failed") {
        return { outcome: WRITE.failed, reason: details.reason ?? response.data?.code ?? response.text };
      }
      await this.refresh();
    }
    throw new Error(`${this.effectName} / ${this.itemName} の selector に追従できません`);
  }

  /**
   * 候補を 1 つ書き込む。
   *
   * **想定しない失敗はここで止める。** 選択肢の書き込みが受理と拒否以外で落ちる
   * のは我々の想定が外れたときであり、続ければ候補の集合が静かに欠ける。値域の
   * 探りとは扱いが違う——あちらは失敗そのものが「測れなかった」という観測である。
   */
  async writeChoice(value, budget) {
    const result = await this.write({ type: "choice", value }, budget);
    if (result.outcome === WRITE.failed) {
      throw new Error(`${this.effectName} / ${this.itemName} への書き込みが失敗しました: ${result.reason}`);
    }
    return result;
  }
}

/**
 * 現在値を起点に在庫の上下へ広げる。
 *
 * 失敗した方向はそこで止める。**同じ項目の選択肢は在庫の中で連続する**ため、
 * 隣が拒否された時点でその側の端に達している。現在値が在庫に無い項目は起点を
 * 取れないため、1 件も試さずに 2 段目へ渡す。
 */
async function expandFromCurrent(target, inventory, positionOf, budget) {
  const found = new Set([target.currentValue]);
  const start = positionOf.get(target.currentValue);
  let writes = 0;
  if (start === undefined) return { found, writes, halted: false };
  for (const step of [-1, 1]) {
    for (let distance = 1; distance <= MAX_STEPS_PER_DIRECTION; distance += 1) {
      const candidate = inventory[start + step * distance];
      if (candidate === undefined) break;
      const result = await target.writeChoice(candidate, budget);
      if (result.outcome === WRITE.exhausted) return { found, writes, halted: true };
      writes += 1;
      if (result.outcome !== WRITE.accepted) break;
      found.add(candidate);
    }
  }
  return { found, writes, halted: false };
}

/**
 * 値域と小数桁を測る。
 *
 * 上限・下限・小数桁のいずれも、**ホストが倒した値が失敗の応答に載ること**で
 * 分かる。受理された探りは何も教えない——その側の端が探りの外にあるだけであり、
 * 端が無いことの証明にはならない。
 *
 * 受理と拒否以外の失敗が返ったら、そこで測るのをやめて理由を持ち帰る。移動を
 * 持つ項目（`track_movement_present`）と、探りの値そのものをホストが解釈できない
 * 場合がここへ来る。**黙って値域を持たない項目にはしない。**
 */
export async function measureRange(target, itemType, budget) {
  const measuresDecimals = DECIMALS_ITEM_TYPES.includes(itemType);
  const probes = [
    ["max", RANGE_PROBE.max],
    ["min", RANGE_PROBE.min],
  ];
  if (measuresDecimals) probes.push(["decimals", DECIMALS_PROBE]);

  const range = { min: null, max: null, decimals: null };
  // 探りが返した表記の小数桁。**測定の根拠が 1 つに絞れることを、複数の観測が
  // 一致することで確かめる。**
  const digits = new Set();
  const unreadable = [];
  let decimalsProbeAccepted = false;
  let writes = 0;
  const outcome = () => ({ range, writes, halted: false, failure: null, unreadable });
  for (const [part, value] of probes) {
    const result = await target.write({ type: itemType, value }, budget);
    if (result.outcome === WRITE.exhausted) {
      return { ...outcome(), halted: true };
    }
    writes += 1;
    if (result.outcome === WRITE.failed) {
      return { ...outcome(), failure: result.reason };
    }
    if (result.outcome === WRITE.accepted) {
      if (part === "decimals") decimalsProbeAccepted = true;
      continue;
    }
    const observedDigits = fractionDigits(result.observed);
    if (observedDigits === null) {
      // 観測はあったが数として読めない。受理されたことと取り違えられないよう、
      // 測れなかった理由として持ち帰る。
      unreadable.push(`${part}: ${result.observed}`);
      continue;
    }
    digits.add(observedDigits);
    if (part !== "decimals") range[part] = observedNumber(result.observed);
  }
  // **小数桁を書くのは、探りを掛けた種別で、桁が 1 つに定まったときだけである。**
  if (measuresDecimals && !decimalsProbeAccepted && digits.size === 1) {
    range.decimals = [...digits][0];
  }
  return outcome();
}

/**
 * ホストが返した表記から小数桁を数える。
 *
 * 数えるのは表記そのものである。ホストは値をその項目の桁で書き出すため、
 * `4000.00` は 2 桁の項目であり `4000` は 0 桁の項目である。数として読めない
 * 表記は測定にならないため `null` を返す。
 */
export function fractionDigits(text) {
  const match = /^-?\d+(?:\.(\d+))?$/.exec(String(text ?? "").trim());
  if (!match) return null;
  return match[1]?.length ?? 0;
}

/** ホストが返した表記を数として読む。読めなければ `null` を返す。 */
export function observedNumber(text) {
  const trimmed = String(text ?? "").trim();
  if (!/^-?\d+(?:\.\d+)?$/.test(trimmed)) return null;
  return Number.parseFloat(trimmed);
}

/**
 * 測れた欄だけを持つ値域を組み立てる。1 つも測れていなければ `null` を返す。
 *
 * **測れなかった欄は書かない。** 書けば「測れなかった」と「そちら側に端が無い」
 * が同じ表記になる。
 */
export function rangeFacet(range) {
  const measured = Object.entries(range).filter(([, value]) => value !== null);
  return measured.length > 0 ? Object.fromEntries(measured) : null;
}

/**
 * 候補の集合から、在庫に在るものだけを在庫の並び順で返す。
 *
 * **在庫に位置を持たない値は落とす。** 位置を持たないのは走査を始めた時点の値
 * だけであり、それが在庫に無いということは、その値が在庫の外から来たという
 * ことである——ホストが持つ既定値なり、利用者の環境に在るファイルなりである。
 * `縁取り / パターン画像` の既定値は空文字列であり、`図形の種類` は svg の
 * ファイル名を取り得る。**残せば基底が環境ごとに違う表になる。**
 *
 * 順を決める材料が無いことも同じ理由から来ている。在庫の外に在る値を並べる
 * 位置は、在庫からは決まらない。
 */
export function inInventoryOrder(values, positionOf) {
  return [...values]
    .filter((value) => positionOf.has(value))
    .sort((left, right) => positionOf.get(left) - positionOf.get(right));
}

/**
 * 効果名・項目名の昇順で並べた表を組み立てる。
 *
 * 葉は面の組である。**軸（効果名・項目名）と内容（`choices` / `range`）で階層を
 * 分ける**ため、面が増えてもトップレベルのキーは増えない。
 */
export function buildDocument(entries) {
  const byName = (left, right) => (left < right ? -1 : left > right ? 1 : 0);
  const effects = {};
  const effectNames = [...new Set(entries.map((entry) => entry.effect))].sort(byName);
  for (const effectName of effectNames) {
    const items = {};
    const own = entries.filter((entry) => entry.effect === effectName);
    for (const itemName of own.map((entry) => entry.item).sort(byName)) {
      items[itemName] = own.find((entry) => entry.item === itemName).facets;
    }
    effects[effectName] = items;
  }
  return { effects };
}

/**
 * 測る組を、オブジェクトの作り方ごとに束ねる。
 *
 * 束が走査の単位であり、**同時に生きるスクラッチの数は束の数ではなく 1 である。**
 * 束の並びも束の中の並びも `pairs` の順そのものにする——報告の並びが走査の順に
 * 従い、途中で止まったときにどこまで進んだかが読める。
 *
 * オブジェクトの作り方が決まらなかった組は束に入らない。呼び出し側が既に
 * 報告へ回している。
 */
export function groupBySource(pairs, hosts) {
  const groups = new Map();
  for (const pair of pairs) {
    const source = hosts.get(pair.effect);
    if (source === undefined) continue;
    const record = {
      ...pair,
      source,
      target: null,
      initialValue: null,
      found: new Set(),
      discriminates: null,
      range: null,
      failure: null,
      unreadable: [],
      blocked: null,
      writes: 0,
      surveyed: false,
    };
    const group = groups.get(source);
    if (group) group.push(record);
    else groups.set(source, [record]);
  }
  return groups;
}

/**
 * 1 つのスクラッチのオブジェクトに乗る組を、段を追って測る。
 *
 * 段の並びは 負の対照 → 境界拡張 → 未割当の回収 → 値域 である。**変わるのは
 * 段が回る範囲だけであり、どの段が何を書くかは束ね方に依らない。**
 *
 * 予算を使い切った段はそこで抜ける。続く段の最初の書き込みも同じく尽きるため、
 * 測り終えなかった組は `surveyed` が偽のまま呼び出し側の報告へ回る。
 */
async function surveyGroup(survey, objectSelector, group, context) {
  const { inventory, positionOf, budget } = context;
  for (const record of group) {
    const target = new Target(survey, objectSelector, record.effect, record.item);
    await target.refresh();
    record.target = target;
    record.initialValue = target.currentValue;
  }
  const choiceRecords = group.filter((record) => record.facet === "choices");
  const rangeRecords = group.filter((record) => record.facet === "range");

  // 0 段目: 負の対照。候補になり得ない文字列を 1 回書き、判定が効く項目だけを
  // 在庫の総当たりへ進める。
  for (const entry of choiceRecords) {
    const result = await entry.target.writeChoice(NEGATIVE_CONTROL, budget);
    if (result.outcome === WRITE.exhausted) break;
    entry.writes += 1;
    entry.discriminates = result.outcome !== WRITE.accepted;
    if (!entry.discriminates) {
      // 在庫を試しても受理の記録が並ぶだけである。ここで終える。
      entry.surveyed = true;
      console.log(`対照 ${entry.effect} / ${entry.item}: 在庫に無い文字列を受理した`);
    }
  }
  const discriminating = choiceRecords.filter((entry) => entry.discriminates === true);

  // 1 段目: 境界拡張。
  for (const entry of discriminating) {
    const result = await expandFromCurrent(entry.target, inventory, positionOf, budget);
    entry.found = result.found;
    entry.writes += result.writes;
    if (result.halted) break;
    console.log(`拡張 ${entry.effect} / ${entry.item}: 書き込み ${result.writes} 回で ${result.found.size} 件`);
  }

  // 2 段目: 1 段目が拾えなかったラベルを項目ごとに残らず試す。
  for (const entry of discriminating) {
    let recovered = 0;
    let halted = false;
    for (const label of inventory) {
      if (entry.found.has(label)) continue;
      const result = await entry.target.writeChoice(label, budget);
      if (result.outcome === WRITE.exhausted) {
        halted = true;
        break;
      }
      entry.writes += 1;
      if (result.outcome !== WRITE.accepted) continue;
      entry.found.add(label);
      recovered += 1;
    }
    if (halted) break;
    entry.surveyed = true;
    if (recovered > 0) console.log(`回収 ${entry.effect} / ${entry.item}: ${recovered} 件`);
  }

  // 値域: 1 項目あたり上限・下限・小数桁の 3 回。
  for (const entry of rangeRecords) {
    const result = await measureRange(entry.target, entry.itemType, budget);
    entry.range = result.range;
    entry.failure = result.failure;
    entry.unreadable = result.unreadable;
    entry.writes += result.writes;
    if (result.halted) break;
    entry.surveyed = true;
    const measured = rangeFacet(result.range);
    if (measured) console.log(`値域 ${entry.effect} / ${entry.item}: ${JSON.stringify(measured)}`);
  }
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  const language = resolveLanguageFile(options.language);
  const inventory = language.inventory;
  const positionOf = new Map(inventory.map((label, index) => [label, index]));
  console.log(`在庫: ${language.path} から ${inventory.length} 件`);

  const budget = new Budget(options.maxWrites);
  const unreached = [];
  const mcp = options.server ? new Mcp(options.server) : new Mcp();
  let survey = null;
  try {
    survey = await Survey.open(mcp);
    const existing = await survey.listObjects();
    const occupied = existing.map((object) => object.selector.layer);
    const layers = new ScratchLayers((occupied.length > 0 ? Math.max(...occupied) : -1) + 1);
    console.log(`既存のオブジェクト ${existing.length} 件。レイヤー ${layers.next} 以降を使います`);

    const effects = await describeAllEffects(survey);
    const byType = {};
    for (const effect of effects) byType[effect.type] = (byType[effect.type] ?? 0) + 1;
    console.log(`効果 ${effects.length} 件 ${JSON.stringify(byType)}`);

    const pairs = effects.flatMap((effect) =>
      effect.items
        .filter((item) => FACET_OF_ITEM_TYPE.has(item.itemType))
        .map((item) => ({
          effect: effect.name,
          item: item.name,
          itemType: item.itemType,
          facet: FACET_OF_ITEM_TYPE.get(item.itemType),
        })),
    );
    const byItemType = {};
    for (const pair of pairs) byItemType[pair.itemType] = (byItemType[pair.itemType] ?? 0) + 1;
    console.log(`測る (効果, 項目): ${pairs.length} 組 ${JSON.stringify(byItemType)}`);

    const targetEffectNames = [...new Set(pairs.map((pair) => pair.effect))];
    const hosts = await resolveHosts(survey, targetEffectNames, effects, layers);
    for (const name of targetEffectNames) {
      if (hosts.has(name)) continue;
      for (const pair of pairs.filter((entry) => entry.effect === name)) {
        unreached.push({ ...pair, reason: "オブジェクトを作れず、他の効果にも同伴しない" });
      }
    }

    // 作り方ごとに束ねる。束の間だけスクラッチのオブジェクトを 1 つ生かす。
    const groups = groupBySource(pairs, hosts);

    let position = 0;
    for (const [sourceName, group] of groups) {
      position += 1;
      if (budget.exhausted) break;
      console.log(`[${position}/${groups.size}] ${sourceName} から ${group.length} 組`);
      const scratch = await withScratchObject(survey, layers, sourceName, (selector) =>
        surveyGroup(survey, selector, group, { inventory, positionOf, budget }),
      );
      if (!scratch.created) {
        for (const record of group) record.blocked = `${sourceName} からオブジェクトを作れない`;
      }
    }

    const complete = [];
    for (const entry of [...groups.values()].flat()) {
      const pair = { effect: entry.effect, item: entry.item };
      if (entry.blocked) {
        unreached.push({ ...pair, reason: entry.blocked });
      } else if (!entry.surveyed) {
        unreached.push({ ...pair, reason: `書き込みの上限 ${budget.limit} 回に達した` });
      } else if (entry.facet === "choices") {
        const values = inInventoryOrder(entry.found, positionOf);
        if (!entry.discriminates) {
          unreached.push({ ...pair, reason: "在庫に無い文字列を受理しており、値を候補の集合に対して解決していない" });
        } else if (values.length <= 1) {
          // 在庫がその項目の選択肢を 1 つも覆っていない。表へ入れても get_object が
          // 既に返す値をなぞるだけで、選べる先を示さない。
          unreached.push({
            ...pair,
            reason: `在庫が覆っておらず、確認できた候補は ${values.length} 件（走査開始時の値は ${entry.initialValue}）`,
          });
        } else {
          complete.push({ ...pair, facets: { choices: values } });
        }
      } else if (entry.failure) {
        unreached.push({ ...pair, reason: `探りが ${entry.failure} で失敗し、値域を測れない` });
      } else {
        const range = rangeFacet(entry.range);
        if (range) complete.push({ ...pair, facets: { range } });
        else if (entry.unreadable.length > 0) {
          unreached.push({ ...pair, reason: `ホストが返した値を数として読めない（${entry.unreadable.join("、")}）` });
        } else {
          unreached.push({ ...pair, reason: "探りの値が値域の内側に収まり、上限も下限も小数桁も測れない" });
        }
      }
    }

    const table = buildDocument(complete);
    writeFileSync(options.output, `${JSON.stringify(table, null, 2)}\n`, "utf8");

    console.log(`\n${options.output} へ ${complete.length} 組を書き出しました（書き込み ${budget.used} 回）`);
    if (unreached.length > 0) {
      console.log(`到達できなかった組 ${unreached.length} 件:`);
      for (const entry of unreached) console.log(`  ${entry.effect} / ${entry.item}: ${entry.reason}`);
    }
  } finally {
    if (survey) {
      const removed = await survey.cleanup();
      console.log(`スクラッチのオブジェクトを ${removed} 件削除しました`);
    }
    mcp.close();
  }
}

// 直接実行されたときだけ走査を始める。**表を組み立てる関数は実機を要さない**
// ため、読み込むだけで小さな入力から出力の形を確かめられるようにする。
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}
