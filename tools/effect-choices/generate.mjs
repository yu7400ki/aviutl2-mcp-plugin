// 選択肢の候補表を、起動中の AviUtl2 への書き込み検証で起こす。
//
// 候補の在庫は言語ファイルの `[Effect]` 節から得る。ただし**どの設定項目の選択肢
// なのかはファイルに書かれていない。** 効果名・項目名・選択肢ラベルが重複排除
// されたまま平坦に並ぶだけである。所属は、スクラッチのオブジェクトへ実際に
// 書き込んで受理されるかどうかで決める。**選択肢に無い値の書き込みは状態を
// 変えない**ため、受理された値だけが残り、スクラッチのオブジェクトを最後に
// 消せば副作用は残らない。
//
// # 対象を select に限る理由
//
// - **combo は集合を確定できない。** リストと文字の複合であり、一覧に無い文字列も
//   受理する（`図形 / 図形の種類` は svg ファイルのパスを受け取る）。受理された
//   ことが所属の証明にならないため、書き込み検証では境界が決まらない。
// - **mask と figure は環境に依る。** 候補はデータディレクトリの内容（`Figure`
//   ディレクトリなど）に由来し、利用者ごとに変わる。配布物へ同梱する静的な表に
//   入れる対象ではない。
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

import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
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

/** 書き込みの総回数の上限。暴走したときにここで止まる。 */
const DEFAULT_MAX_WRITES = 50000;

/** 境界拡張で 1 方向へ進む最大の歩数。 */
const MAX_STEPS_PER_DIRECTION = 200;

/** describe_effects が 1 度に受け取れる効果の数。 */
const DESCRIBE_BATCH = 10;

/** 走査する効果の種別。 */
const EFFECT_TYPES = ["input", "output", "control", "filter", "transition"];

/** 対象とする設定項目の種別。 */
const TARGET_ITEM_TYPE = "select";

/** 書き込みが受理されなかったことを表す応答の理由。 */
const NOT_APPLIED = "item_value_not_applied";

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

  /** 自分が作ったオブジェクトを全て消す。1 件の失敗で他を諦めない。 */
  async cleanup() {
    let removed = 0;
    for (const selector of this.created) {
      try {
        const object = await this.getObject(selector);
        const response = await this.call("delete_object", { selector: object.summary.selector });
        if (!response.isError) removed += 1;
      } catch {
        // 既に消えているオブジェクトは片付ける対象ではない。
      }
    }
    this.created = [];
    return removed;
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
async function resolveHosts(survey, targetNames, effects, layerOf) {
  const hosts = new Map();
  const tried = new Set();

  const probe = async (sourceName) => {
    if (tried.has(sourceName)) return;
    tried.add(sourceName);
    const selector = await survey.createObject(sourceName, layerOf());
    if (!selector) return;
    const object = await survey.getObject(selector);
    for (const effect of object.effects) {
      if (!hosts.has(effect.name)) hosts.set(effect.name, sourceName);
    }
    await survey.call("delete_object", { selector: object.summary.selector });
    survey.created = survey.created.filter((entry) => entry !== selector);
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
   * 候補を 1 つ書き込み、受理されたかどうかを返す。
   *
   * **拒否は状態を変えない**ため、selector はそのまま次の試行に使える。受理された
   * ときだけ応答が返す新しい selector へ差し替える。
   */
  async write(value, budget) {
    if (!budget.take()) return null;
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const response = await this.survey.call("set_object_item", {
        selector: this.effectSelector,
        item: this.itemName,
        value: { type: "choice", value },
      });
      if (!response.isError) {
        this.effectSelector = response.data.effect.selector;
        this.objectSelector = response.data.effect.selector.object;
        this.currentValue = value;
        return true;
      }
      const details = response.data?.details ?? {};
      if (details.reason === NOT_APPLIED) return false;
      if (response.data?.code !== "precondition_failed") {
        throw new Error(`${this.effectName} / ${this.itemName} への書き込みが失敗しました: ${response.text}`);
      }
      await this.refresh();
    }
    throw new Error(`${this.effectName} / ${this.itemName} の selector に追従できません`);
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
      const accepted = await target.write(candidate, budget);
      if (accepted === null) return { found, writes, halted: true };
      writes += 1;
      if (!accepted) break;
      found.add(candidate);
    }
  }
  return { found, writes, halted: false };
}

/**
 * 候補の集合を在庫の並び順に整える。
 *
 * 在庫に位置を持たない値は先頭へ置く。位置を持たないのは走査を始めた時点の
 * 現在値だけであり、他に順を決める材料が無い。
 */
function inInventoryOrder(values, positionOf) {
  const position = (value) => positionOf.get(value) ?? -1;
  return [...values].sort((left, right) => position(left) - position(right));
}

/** 効果名・項目名の昇順で並べた表を組み立てる。 */
function buildDocument(entries) {
  const byName = (left, right) => (left < right ? -1 : left > right ? 1 : 0);
  const effects = {};
  const effectNames = [...new Set(entries.map((entry) => entry.effect))].sort(byName);
  for (const effectName of effectNames) {
    const items = {};
    const own = entries.filter((entry) => entry.effect === effectName);
    for (const itemName of own.map((entry) => entry.item).sort(byName)) {
      items[itemName] = own.find((entry) => entry.item === itemName).values;
    }
    effects[effectName] = items;
  }
  return { effects };
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
    const layers = existing.map((object) => object.selector.layer);
    let nextLayer = (layers.length > 0 ? Math.max(...layers) : -1) + 1;
    const layerOf = () => nextLayer++;
    console.log(`既存のオブジェクト ${existing.length} 件。レイヤー ${nextLayer} 以降を使います`);

    const effects = await describeAllEffects(survey);
    const byType = {};
    for (const effect of effects) byType[effect.type] = (byType[effect.type] ?? 0) + 1;
    console.log(`効果 ${effects.length} 件 ${JSON.stringify(byType)}`);

    const pairs = effects.flatMap((effect) =>
      effect.items
        .filter((item) => item.itemType === TARGET_ITEM_TYPE)
        .map((item) => ({ effect: effect.name, item: item.name })),
    );
    console.log(`${TARGET_ITEM_TYPE} の (効果, 項目): ${pairs.length} 組`);

    const targetEffectNames = [...new Set(pairs.map((pair) => pair.effect))];
    const hosts = await resolveHosts(survey, targetEffectNames, effects, layerOf);
    for (const name of targetEffectNames) {
      if (hosts.has(name)) continue;
      for (const pair of pairs.filter((entry) => entry.effect === name)) {
        unreached.push({ ...pair, reason: "オブジェクトを作れず、他の効果にも同伴しない" });
      }
    }

    // 対象の効果を載せたオブジェクトを、作り方ごとに 1 つずつ用意する。
    const objects = new Map();
    for (const sourceName of new Set([...hosts.values()])) {
      const selector = await survey.createObject(sourceName, layerOf());
      if (selector) objects.set(sourceName, selector);
    }

    const targets = [];
    for (const pair of pairs) {
      const sourceName = hosts.get(pair.effect);
      const selector = sourceName ? objects.get(sourceName) : null;
      if (!selector) {
        if (sourceName) unreached.push({ ...pair, reason: `${sourceName} からオブジェクトを作れない` });
        continue;
      }
      const target = new Target(survey, selector, pair.effect, pair.item);
      await target.refresh();
      targets.push({ ...pair, target, initialValue: target.currentValue, found: new Set(), writes: 0, surveyed: false });
    }

    // 1 段目: 境界拡張。
    for (const entry of targets) {
      const result = await expandFromCurrent(entry.target, inventory, positionOf, budget);
      entry.found = result.found;
      entry.writes += result.writes;
      if (result.halted) break;
      console.log(`拡張 ${entry.effect} / ${entry.item}: 書き込み ${result.writes} 回で ${result.found.size} 件`);
    }

    // 2 段目: 1 段目が拾えなかったラベルを項目ごとに残らず試す。
    for (const entry of targets) {
      let recovered = 0;
      let halted = false;
      for (const label of inventory) {
        if (entry.found.has(label)) continue;
        const accepted = await entry.target.write(label, budget);
        if (accepted === null) {
          halted = true;
          break;
        }
        entry.writes += 1;
        if (!accepted) continue;
        entry.found.add(label);
        recovered += 1;
      }
      if (halted) break;
      entry.surveyed = true;
      if (recovered > 0) console.log(`回収 ${entry.effect} / ${entry.item}: ${recovered} 件`);
    }

    const complete = [];
    for (const entry of targets) {
      const pair = { effect: entry.effect, item: entry.item };
      if (!entry.surveyed) {
        unreached.push({ ...pair, reason: `書き込みの上限 ${budget.limit} 回に達した` });
      } else if (entry.found.size <= 1) {
        // 現在値しか残らなかった項目は、在庫がその選択肢を 1 つも覆っていない。
        // 表へ入れても get_object が既に返す値をなぞるだけで、選べる先を示さない。
        unreached.push({ ...pair, reason: `在庫が覆っておらず、${entry.initialValue} 以外を確認できない` });
      } else {
        complete.push(entry);
      }
    }

    const table = buildDocument(
      complete.map((entry) => ({
        effect: entry.effect,
        item: entry.item,
        values: inInventoryOrder(entry.found, positionOf),
      })),
    );
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

await main();
