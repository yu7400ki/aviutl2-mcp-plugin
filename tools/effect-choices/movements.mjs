// 移動方法の名前ごとの可否——その名前で移動を書けるか——を、起動中の
// AviUtl2 への書き込み検証で起こす。
//
// **一覧に載るのに書けない名前がある。** 名前としては正しく、書き込みも発行され
// るが、読み直しがその移動を失う。移動を消す正解は移動方法を指定しない指定
// （`mode` が `null`）である。**名前をコードへ埋め込まず、走査で起こす**——
// 移動方法の集合は環境ごとに違い、同じ性質の名前が他に無いとは言えない。
//
// # 判定基準は「読み直しが移動を失うこと」である
//
// 書き込み検証が既に行っている照合そのものであり、新しい規則を書かない。
// 移動を持つ値を書いて受理されれば書ける名前であり、`item_value_not_applied`
// で落ちたうえで読み直した値が区切りを持たない単一の数値になっていれば、
// 移動が失われている（[`observedHasMovement`]）。
//
// **移動が失われたと言えるのは、単一の数値を読んだときだけである。** 読み直した
// 値が移動を持っていれば、違うのは値かパラメータであって移動の有無ではない。
// 読めない表記も同じ扱いにする——どちらも「書けない」の証拠にならず、表に載せず
// 理由付きで報告へ回す。
//
// # 「書けない」は区間の数を変えた 2 回の観測で確かめる
//
// **`writable: false` はバイナリへ焼き込まれ、以後その名前は全オブジェクトで
// 拒まれる。** 1 回の観測では 2 つの誤りを分けられない——その 1 回だけホストが
// 書き込みを無視した場合と、**区間の境界より多くの制御点を要する移動方法**の
// 場合である。後者は区間が多いオブジェクトでなら書けるのに、全域で拒まれる。
//
// そこで 1 回目で移動を失った名前だけを、スクラッチへ中間点を 1 つ足してから
// 測り直す。両方が失えば記録し、区間の数で結果が変わるなら**その性質は名前に
// ついての表に書けない**ため報告へ回す。**受理された側は補強を要さない**——
// 書き戻し照合が値・移動方法・フラグ整数の構造一致を要求するためである。
//
// # 1 件の失敗で走査を止めない
//
// 名前 1 件の測定は例外まで含めて [`attempt`] の中に閉じる。ホストは移動の
// パラメータを補って fingerprint を変えるため、追従しきれずに落ちる経路が実在
// する。走査ごと止めれば、書き出しはループの後にあるためそれまでに測った全件が
// 失われる。**走査を止めるのは、測定を 1 件も始められないときだけである。**
//
// # 測る前に移動を消す
//
// 名前を 1 つ測るたび、まず移動を消して静的な値へ戻す。**書ける名前を測った後は
// その移動が残る**ため、続く名前をそのまま測ると判定の土台が名前ごとに変わる。
// 移動を持つ状態から測ると、ホストが書き込みを無視して前の移動を保った場合に
// 「移動を失っていない」と読めてしまい、書けない名前が表から落ちる。静的な値
// から測れば、ホストが値を捨てても無視しても、読み直しは単一の数値になる。
//
// # 一覧は失敗の応答から引く
//
// 移動方法の一覧を返す読み取りは無い。一覧に無い名前を書くと、失敗の
// `details.known_movements` にその環境の移動方法が並ぶ。**この失敗は書き込みを
// 発行しない。** 値の個数の検証が名前の検証より先に来るため、区間の数を読んで
// から送る。
//
// **上限で切られたときは応答がそう名乗る。** 切る前の件数は `details.truncated`
// が運ぶ（[`truncatedTotal`]）。届かなかった名前は測れないため、そのことを
// 数で警告する。
//
// # 走らせ直すときの循環
//
// **表が既に埋まっていると測れない。** plugin は表を読み、書けないと述べている
// 名前への書き込みを発行の前に拒む（`track_mode_not_writable`）。その状態で
// 走らせれば、走査は自分の前回の出力を写すだけになる。そこで
// [`tableIsEmpty`] が空でなければ走査を始めない。

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { Mcp, REPOSITORY } from "./mcp.mjs";

/**
 * plugin が `include_str!` で取り込む可否の表。
 *
 * 既定の書き出し先であり、**循環の判定もここを見る。** 書き出し先を `--out` で
 * 逸らしても、配置されている plugin が読むのはこのファイルから焼き込まれた表で
 * ある。
 */
const EMBEDDED_FACETS = join(REPOSITORY, "crates", "plugin", "data", "movement_facets.json");

/** 測る対象の効果。 */
const TARGET_EFFECT = "標準描画";

/** 測る対象のトラックバー項目。 */
const TARGET_ITEM = "X";

/**
 * 区間の境界へ順に置く値。
 *
 * **隣り合う境界に違う値を置く。** 同じ値を並べると、移動が消えたのか保たれた
 * のかを読み直しから見分けられない。
 */
const BOUNDARY_VALUES = [0, 100];

/**
 * 一覧を引くために送る、移動方法として在り得ない名前。
 *
 * 移動方法の名前は `aviutl2.ini` の節の見出しから来るため、`[` と `]` を含む
 * 名前は現れない。
 */
const UNKNOWN_MODE = "[aviutl2-mcp]移動方法ではない名前";

/** 失敗の応答が移動方法の一覧を運ぶ key。切り詰めを名乗る位置でもある。 */
const KNOWN_MOVEMENTS_KEY = "known_movements";

/** 失敗の応答が、上限で切った配列を名乗る key。 */
const TRUNCATED_KEY = "truncated";

/** 一覧に無い名前を書いたときの失敗の理由。 */
const MODE_UNKNOWN = "track_mode_unknown";

/** 表が既に書けないと述べている名前を書いたときの失敗の理由。 */
const MODE_NOT_WRITABLE = "track_mode_not_writable";

/** 書き込みが受理されなかったことを表す応答の理由。 */
const NOT_APPLIED = "item_value_not_applied";

/** 移動を持たない値の表記。区切りを持たない単一の数値である。 */
const LONE_NUMBER = /^-?\d+(?:\.\d+)?$/;

/** 測定 1 回の結末。 */
export const WRITE = {
  /** ホストが要求どおりの移動を持った。 */
  accepted: "accepted",
  /** ホストが値を倒したか捨てた。読み直した値が `observed` に入る。 */
  notApplied: "not_applied",
  /** 上記以外の失敗。ホストが名乗った理由が `reason` に入る。 */
  failed: "failed",
  /** 測る土台を作れなかった。妨げた結末の説明が `reason` に入る。 */
  unprepared: "unprepared",
  /** 測定が例外で落ちた。文面が `reason` に入る。 */
  raised: "raised",
};

/**
 * 応答が名乗った、切り詰める前の要素数。名乗っていなければ `null`。
 *
 * 上限で切った配列については、失敗の応答が切られた位置と切る前の件数を
 * [`TRUNCATED_KEY`] の下へ並べる。**上限がいくつかを知らなくてよい**——切ったと
 * いう事実も件数も応答が運ぶ。
 *
 * `path` は切られた配列の位置である。トップレベルの配列ではその key そのものに
 * なる。
 */
export function truncatedTotal(details, path) {
  const total = details?.[TRUNCATED_KEY]?.[path];
  return Number.isInteger(total) && total > 0 ? total : null;
}

/**
 * 読み直した値が移動を持つか。
 *
 * **移動を失ったと言えるのは、区切りを持たない単一の数値を読んだときだけで
 * ある。** 区切りを持つ表記は移動を保っており、数として読めない表記は測定に
 * ならない。どちらも「移動を失った」の証拠にはならないため、同じ側へ倒す。
 */
export function observedHasMovement(observed) {
  return !LONE_NUMBER.test(String(observed ?? "").trim());
}

/** その結末が「移動を失った」という観測か。 */
export function lostMovement(result) {
  return result.outcome === WRITE.notApplied && !observedHasMovement(result.observed);
}

/** その結末が「移動を失わなかった」という観測か。 */
function keptMovement(result) {
  return (
    result.outcome === WRITE.accepted ||
    (result.outcome === WRITE.notApplied && observedHasMovement(result.observed))
  );
}

/** 説明へ、名乗られた理由を括弧で添える。名乗られていなければ何も添えない。 */
function withReason(text, reason) {
  return typeof reason === "string" && reason.length > 0 ? `${text}（${reason}）` : text;
}

/** 測定 1 回の結末を 1 行で述べる。 */
function describeWrite(result) {
  switch (result.outcome) {
    case WRITE.accepted:
      return "受理された";
    case WRITE.notApplied:
      return `${NOT_APPLIED}: ${result.observed}`;
    default:
      return withReason("失敗した", result.reason);
  }
}

/** 移動を失ったという観測にならなかった結末を、表に載せない理由として述べる。 */
function droppedReason(result) {
  switch (result.outcome) {
    case WRITE.notApplied:
      return `読み直した値が移動を失っていない（${result.observed}）`;
    case WRITE.unprepared:
      return withReason("測る土台を作れなかった", result.reason);
    case WRITE.raised:
      return withReason("測定が例外で落ちた", result.reason);
    default:
      return result.reason === MODE_NOT_WRITABLE
        ? `${MODE_NOT_WRITABLE} で書き込みの発行前に拒まれた。配置されている plugin が読む表が古い可能性がある`
        : withReason("書き込みが失敗した", result.reason);
  }
}

/**
 * 名前 1 件について集めた結末から、表へどう載せるかを決める。
 *
 * `first` は区間の数がそのままの測定、`second` は中間点を足して区間を増やして
 * から測り直した結果である。**結末の振り分けはここに集める**——測定の側は結末を
 * 組み立てるだけで、表への載せ方を決めない。
 *
 * **`writable: false` は 2 回の観測が揃って初めて記録する。** 焼き込まれた偽は
 * その名前を全オブジェクトで拒むため、1 回の観測では確定させない。区間の数で
 * 結果が変わる名前は、名前についての表に書ける性質を持たない。
 *
 * **覆えなかった名前は表に入らない。** 「測っていない」を表す形が表に無いため、
 * 判定が付かなかった名前は理由を添えて呼び出し側の報告へ回す。
 */
export function verdict(first, second = null) {
  if (first.outcome === WRITE.accepted) {
    return { recorded: true, writable: true };
  }
  if (!lostMovement(first)) {
    return { recorded: false, reason: droppedReason(first) };
  }
  if (second === null) {
    return { recorded: false, reason: "移動を失ったが、区間の数を変えて確かめ直していない" };
  }
  if (lostMovement(second)) {
    return { recorded: true, writable: false };
  }
  if (keptMovement(second)) {
    return {
      recorded: false,
      reason: `区間を増やして測り直すと移動を失わなかった（${describeWrite(second)}）。区間の数で結果が変わる性質を、名前の表には書けない`,
    };
  }
  return { recorded: false, reason: `区間を増やした確かめ直しを測れなかった——${droppedReason(second)}` };
}

/**
 * 表へ載せると決めた名前だけを、名前の昇順で並べた文書を組み立てる。
 *
 * 昇順に並べるのは差分を読めるようにするためである。
 */
export function buildDocument(measurements) {
  const byName = (left, right) => (left < right ? -1 : left > right ? 1 : 0);
  const recorded = measurements.filter((entry) => entry.verdict.recorded);
  const movements = {};
  for (const name of recorded.map((entry) => entry.name).sort(byName)) {
    movements[name] = { writable: recorded.find((entry) => entry.name === name).verdict.writable };
  }
  return { movements };
}

/**
 * 可否の表が空か。
 *
 * 空でない表は走査の土台にならない。**読めない表も空とは呼ばない**——直し方は
 * どちらも同じであり、空へ戻してから走らせ直すことになる。
 */
export function tableIsEmpty(source) {
  let document;
  try {
    document = JSON.parse(source);
  } catch {
    return false;
  }
  const movements = document?.movements;
  if (movements === null || typeof movements !== "object" || Array.isArray(movements)) {
    return false;
  }
  return Object.keys(movements).length === 0;
}

/** 引数を解釈する。 */
function parseArguments(argv) {
  const options = { output: EMBEDDED_FACETS, server: null };
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    const value = argv[index + 1];
    if (value === undefined) throw new Error(`${key} に値がありません`);
    if (key === "--out") options.output = value;
    else if (key === "--server") options.server = value;
    else throw new Error(`知らない引数です: ${key}`);
  }
  return options;
}

/** tool の応答から構造化出力を取り出す。失敗は例外にする。 */
function expect(response, what) {
  if (response.isError) throw new Error(`${what}を取得できません: ${response.text}`);
  return response.data;
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

/** スクラッチのオブジェクトが載せる、測る対象の設定項目。 */
class Item {
  constructor(survey, objectSelector) {
    this.survey = survey;
    this.objectSelector = objectSelector;
    this.effectSelector = null;
    this.sectionCount = 0;
  }

  /** オブジェクトを読み直し、対象の効果と区間の数を取り直す。 */
  async refresh() {
    const object = await this.survey.getObject(this.objectSelector);
    this.objectSelector = object.summary.selector;
    this.sectionCount = object.sections.length;
    const effect = object.effects.find((entry) => entry.name === TARGET_EFFECT);
    if (!effect) throw new Error(`${TARGET_EFFECT} がオブジェクトから消えています`);
    if (!effect.items.some((entry) => entry.name === TARGET_ITEM)) {
      throw new Error(`${TARGET_EFFECT} に ${TARGET_ITEM} がありません`);
    }
    this.effectSelector = effect.selector;
  }

  /** 移動を持つ値。境界の数は区間数 + 1 である。 */
  movingValue(mode) {
    const values = [];
    for (let index = 0; index <= this.sectionCount; index += 1) {
      values.push(BOUNDARY_VALUES[index % BOUNDARY_VALUES.length]);
    }
    return { type: "track", values, mode, params: [], accelerate: false, decelerate: false, twopoint: false };
  }

  /** 移動を持たない値。測る前の土台にする。 */
  staticValue() {
    return {
      type: "track",
      values: [BOUNDARY_VALUES[0]],
      mode: null,
      params: [],
      accelerate: false,
      decelerate: false,
      twopoint: false,
    };
  }

  /**
   * 値を 1 つ書き込み、結末を返す。
   *
   * **拒否は状態を変えず、倒しは書き込み前の値へ巻き戻される**ため、selector は
   * そのまま次の試行に使える。受理されたときだけ応答が返す新しい selector へ
   * 差し替える。
   */
  async write(value) {
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const response = await this.survey.call("set_object_item", {
        selector: this.effectSelector,
        item: TARGET_ITEM,
        value,
      });
      if (!response.isError) {
        this.effectSelector = response.data.effect.selector;
        this.objectSelector = response.data.effect.selector.object;
        return { outcome: WRITE.accepted };
      }
      const details = response.data?.details ?? {};
      if (details.reason === NOT_APPLIED) {
        return { outcome: WRITE.notApplied, observed: details.observed_value ?? null };
      }
      if (response.data?.code !== "precondition_failed") {
        return {
          outcome: WRITE.failed,
          reason: details.reason ?? response.data?.code ?? response.text,
          details,
        };
      }
      await this.refresh();
    }
    throw new Error(`${TARGET_EFFECT} / ${TARGET_ITEM} の selector に追従できません`);
  }
}

/** オブジェクトを作れる効果の名前を列挙する。 */
async function listSourceEffects(survey) {
  const names = [];
  let offset = 0;
  for (;;) {
    const page = expect(
      await survey.call("list_available_effects", { effect_type: "input", offset, limit: 200 }),
      "input の効果の一覧",
    );
    for (const item of page.items) names.push(item.name);
    if (!page.page.has_more) return names;
    offset = page.page.next_offset;
  }
}

/**
 * 対象の設定項目を載せたオブジェクトを 1 つ作る。
 *
 * **作り方を決め打ちしない。** 効果からオブジェクトを作れるかは実際に作って
 * 確かめ、対象の効果が付いてこなかったオブジェクトはその場で消す。
 */
async function createScratch(survey, layer) {
  let target = layer;
  for (const name of await listSourceEffects(survey)) {
    const selector = await survey.createObject(name, target);
    if (!selector) continue;
    const object = await survey.getObject(selector);
    const effect = object.effects.find((entry) => entry.name === TARGET_EFFECT);
    if (effect?.items.some((entry) => entry.name === TARGET_ITEM)) {
      console.log(`${name} から作ったオブジェクトで測ります`);
      return selector;
    }
    // 消せなかったオブジェクトのレイヤーは占有されたままである。次の試行は
    // その先へ置く。
    if (!(await survey.destroyObject(selector))) target += 1;
  }
  throw new Error(`${TARGET_EFFECT} / ${TARGET_ITEM} を載せたオブジェクトを作れません`);
}

/**
 * 移動方法の名前を引き、受け取った名前と、応答が名乗った全件数を返す。
 *
 * 一覧に無い名前を書いた失敗が一覧を運ぶ。**この失敗は書き込みを発行しない。**
 *
 * `total` は応答が切り詰めを名乗ったときだけ数を持つ。切られていなければ
 * `null` であり、受け取った名前が全件である。
 */
async function readMovementNames(item) {
  const result = await item.write(item.movingValue(UNKNOWN_MODE));
  if (result.outcome !== WRITE.failed || result.reason !== MODE_UNKNOWN) {
    throw new Error(`一覧を引く要求が ${MODE_UNKNOWN} で落ちませんでした: ${describeWrite(result)}`);
  }
  const known = result.details?.[KNOWN_MOVEMENTS_KEY];
  if (!Array.isArray(known) || known.some((entry) => typeof entry?.name !== "string")) {
    throw new Error(`失敗の応答が移動方法の一覧を運んでいません: ${JSON.stringify(result.details)}`);
  }
  if (known.length === 0) {
    throw new Error("移動方法の一覧が空です。plugin が移動方法を 1 つも読めていません");
  }
  return {
    names: known.map((entry) => entry.name),
    total: truncatedTotal(result.details, KNOWN_MOVEMENTS_KEY),
  };
}

/**
 * 名前を 1 つ測り、結末を返す。
 *
 * **測る前に移動を消して静的な値へ戻す。** 土台を名前ごとに変えないためである。
 *
 * **失敗を結末へ畳み、外へ投げない。** 1 件の測定が落ちても走査は次の名前へ
 * 進む。落ちた対象は取り直しておく——追従が済んでいれば次の名前は測れる。
 */
async function attempt(item, name) {
  try {
    const reset = await item.write(item.staticValue());
    if (reset.outcome !== WRITE.accepted) {
      return { outcome: WRITE.unprepared, reason: describeWrite(reset) };
    }
    return await item.write(item.movingValue(name));
  } catch (error) {
    try {
      await item.refresh();
    } catch {
      // 取り直せない対象は、次の名前の測定が同じ形で結末に畳む。
    }
    return { outcome: WRITE.raised, reason: error.message };
  }
}

/**
 * スクラッチへ中間点を 1 つ足し、区間を増やす。妨げがあればその説明を返す。
 *
 * **オブジェクトを増やさない。** 同時に timeline へ乗るスクラッチが 1 つである
 * 限り、1 編集あたりの描画費用は走査の規模に依らない。
 */
async function addSection(survey, item) {
  const object = await survey.getObject(item.objectSelector);
  const [span] = object.sections;
  if (!span) return "オブジェクトの区間を読めない";
  const frame = Math.floor((span.start + span.end) / 2);
  if (frame <= span.start) return "オブジェクトが短く、中間点を置けるフレームが無い";
  const response = await survey.call("create_object_section", {
    selector: object.summary.selector,
    frame,
  });
  if (response.isError) {
    return response.data?.details?.reason ?? response.data?.code ?? response.text;
  }
  await item.refresh();
  return null;
}

/** 決まった載せ方を 1 行で述べる。 */
function summarize(decision) {
  if (!decision.recorded) return decision.reason;
  return decision.writable ? "書ける" : "書けない";
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  if (!tableIsEmpty(readFileSync(EMBEDDED_FACETS, "utf8"))) {
    throw new Error(
      `${EMBEDDED_FACETS} が空ではありません。plugin はこの表を読んで書き込みを発行の前に拒むため、走査は前回の出力を写すだけになります。表を {"movements": {}} へ戻し、plugin をビルドし直してから走らせ直してください`,
    );
  }

  const mcp = options.server ? new Mcp(options.server) : new Mcp();
  let survey = null;
  try {
    survey = await Survey.open(mcp);
    const existing = await survey.listObjects();
    const occupied = existing.map((object) => object.selector.layer);
    const layer = (occupied.length > 0 ? Math.max(...occupied) : -1) + 1;
    console.log(`既存のオブジェクト ${existing.length} 件。レイヤー ${layer} を使います`);

    const item = new Item(survey, await createScratch(survey, layer));
    await item.refresh();
    console.log(`区間 ${item.sectionCount} 個。境界ごとに ${item.sectionCount + 1} 個の値を書きます`);

    const { names, total } = await readMovementNames(item);
    console.log(`移動方法 ${names.length} 件`);
    if (total !== null) {
      console.log(
        `応答は ${total} 件のうち ${names.length} 件だけを運んでいます。届かなかった ${total - names.length} 件は測れません`,
      );
    }

    const measurements = [];
    for (const name of names) {
      const first = await attempt(item, name);
      measurements.push({ name, first, second: null });
      console.log(
        `${name}: ${lostMovement(first) ? "移動を失った。区間の数を変えて確かめ直します" : summarize(verdict(first))}`,
      );
    }

    // 移動を失った名前だけを、区間の数を変えて測り直す。中間点を足すのは
    // 走査を通して 1 度だけである。
    const pending = measurements.filter((entry) => lostMovement(entry.first));
    if (pending.length > 0) {
      console.log(`\n移動を失った ${pending.length} 件を確かめ直します`);
      let obstacle = null;
      try {
        obstacle = await addSection(survey, item);
      } catch (error) {
        obstacle = error.message;
      }
      if (obstacle) console.log(`中間点を追加できません: ${obstacle}`);
      else console.log(`区間 ${item.sectionCount} 個。境界ごとに ${item.sectionCount + 1} 個の値を書きます`);
      for (const entry of pending) {
        entry.second = obstacle
          ? { outcome: WRITE.unprepared, reason: obstacle }
          : await attempt(item, entry.name);
        console.log(`${entry.name}: ${summarize(verdict(entry.first, entry.second))}`);
      }
    }

    const judged = measurements.map((entry) => ({
      name: entry.name,
      verdict: verdict(entry.first, entry.second),
    }));
    const table = buildDocument(judged);
    writeFileSync(options.output, `${JSON.stringify(table, null, 2)}\n`, "utf8");
    console.log(`\n${options.output} へ ${Object.keys(table.movements).length} 件を書き出しました`);

    const unreached = judged.filter((entry) => !entry.verdict.recorded);
    if (unreached.length > 0) {
      console.log(`表に載せなかった名前 ${unreached.length} 件:`);
      for (const entry of unreached) console.log(`  ${entry.name}: ${entry.verdict.reason}`);
    }
  } finally {
    if (survey) {
      const removed = await survey.cleanup();
      console.log(`スクラッチのオブジェクトを ${removed} 件削除しました`);
    }
    mcp.close();
  }
}

// 直接実行されたときだけ走査を始める。**判定と組み立ては実機を要さない**ため、
// 読み込むだけで小さな入力から確かめられるようにする。
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}
