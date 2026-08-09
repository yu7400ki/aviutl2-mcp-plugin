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
// `details.known_movements` にその環境の全件が並ぶ。**この失敗は書き込みを
// 発行しない。** 値の個数の検証が名前の検証より先に来るため、区間の数を読んで
// から送る。
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

/**
 * 失敗の応答が配列として運ぶ要素の数の上限。
 *
 * 超えた分は黙って落ちる。一覧がちょうどこの件数で返ったときは、その先に名前が
 * 在っても届いていない可能性がある。
 */
const DETAIL_ARRAY_LIMIT = 32;

/** 一覧に無い名前を書いたときの失敗の理由。 */
const MODE_UNKNOWN = "track_mode_unknown";

/** 表が既に書けないと述べている名前を書いたときの失敗の理由。 */
const MODE_NOT_WRITABLE = "track_mode_not_writable";

/** 書き込みが受理されなかったことを表す応答の理由。 */
const NOT_APPLIED = "item_value_not_applied";

/** 移動を持たない値の表記。区切りを持たない単一の数値である。 */
const LONE_NUMBER = /^-?\d+(?:\.\d+)?$/;

/** 書き込み 1 回の結末。 */
export const WRITE = {
  /** ホストが要求どおりの移動を持った。 */
  accepted: "accepted",
  /** ホストが値を倒したか捨てた。読み直した値が `observed` に入る。 */
  notApplied: "not_applied",
  /** 上記以外の失敗。ホストが名乗った理由が `reason` に入る。 */
  failed: "failed",
};

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

/**
 * 書き込み 1 回の結末から、その名前を表へどう載せるかを決める。
 *
 * **覆えなかった名前は表に入らない。** 「測っていない」を表す形が表に無いため、
 * 判定が付かなかった名前は理由を添えて呼び出し側の報告へ回す。
 */
export function verdict(result) {
  if (result.outcome === WRITE.accepted) {
    return { recorded: true, writable: true };
  }
  if (result.outcome === WRITE.notApplied) {
    if (observedHasMovement(result.observed)) {
      return {
        recorded: false,
        reason: `読み直した値が移動を失っていない（${result.observed}）`,
      };
    }
    return { recorded: true, writable: false };
  }
  if (result.reason === MODE_NOT_WRITABLE) {
    return {
      recorded: false,
      reason: `${MODE_NOT_WRITABLE} で書き込みの発行前に拒まれた。配置されている plugin が読む表が古い可能性がある`,
    };
  }
  return { recorded: false, reason: `書き込みが ${result.reason} で失敗した` };
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

/** 書き込みの結末を 1 行で述べる。 */
function describeWrite(result) {
  if (result.outcome === WRITE.notApplied) return `${NOT_APPLIED}: ${result.observed}`;
  return String(result.reason);
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
  for (const name of await listSourceEffects(survey)) {
    const selector = await survey.createObject(name, layer);
    if (!selector) continue;
    const object = await survey.getObject(selector);
    const effect = object.effects.find((entry) => entry.name === TARGET_EFFECT);
    if (effect?.items.some((entry) => entry.name === TARGET_ITEM)) {
      console.log(`${name} から作ったオブジェクトで測ります`);
      return selector;
    }
    await survey.destroyObject(selector);
  }
  throw new Error(`${TARGET_EFFECT} / ${TARGET_ITEM} を載せたオブジェクトを作れません`);
}

/**
 * 移動方法の名前を全件引く。
 *
 * 一覧に無い名前を書いた失敗が一覧を運ぶ。**この失敗は書き込みを発行しない。**
 */
async function readMovementNames(item) {
  const result = await item.write(item.movingValue(UNKNOWN_MODE));
  if (result.outcome !== WRITE.failed || result.reason !== MODE_UNKNOWN) {
    throw new Error(`一覧を引く要求が ${MODE_UNKNOWN} で落ちませんでした: ${describeWrite(result)}`);
  }
  const known = result.details?.known_movements;
  if (!Array.isArray(known) || known.some((entry) => typeof entry?.name !== "string")) {
    throw new Error(`失敗の応答が移動方法の一覧を運んでいません: ${JSON.stringify(result.details)}`);
  }
  if (known.length === 0) {
    throw new Error("移動方法の一覧が空です。plugin が移動方法を 1 つも読めていません");
  }
  return known.map((entry) => entry.name);
}

/** 名前を 1 つ測る。土台を作れなければ、その名前を表に載せない。 */
async function measure(item, name) {
  const reset = await item.write(item.staticValue());
  if (reset.outcome !== WRITE.accepted) {
    return { recorded: false, reason: `測る前に移動を消せなかった（${describeWrite(reset)}）` };
  }
  return verdict(await item.write(item.movingValue(name)));
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

    const names = await readMovementNames(item);
    console.log(`移動方法 ${names.length} 件`);
    if (names.length === DETAIL_ARRAY_LIMIT) {
      console.log(
        `一覧が ${DETAIL_ARRAY_LIMIT} 件ちょうどです。失敗の応答はこの件数で配列を切るため、この先の名前は届いていない可能性があります`,
      );
    }

    const measurements = [];
    for (const name of names) {
      const decision = await measure(item, name);
      measurements.push({ name, verdict: decision });
      const summary = decision.recorded
        ? decision.writable
          ? "書ける"
          : "書けない"
        : decision.reason;
      console.log(`${name}: ${summary}`);
    }

    const table = buildDocument(measurements);
    writeFileSync(options.output, `${JSON.stringify(table, null, 2)}\n`, "utf8");
    console.log(`\n${options.output} へ ${Object.keys(table.movements).length} 件を書き出しました`);

    const unreached = measurements.filter((entry) => !entry.verdict.recorded);
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
