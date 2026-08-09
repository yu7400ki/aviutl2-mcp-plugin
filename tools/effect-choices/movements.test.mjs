// 移動方法の可否の生成器のうち、実機を要さない部分を確かめる。
//
// 走査そのものは起動中の AviUtl2 を要するが、**判定と組み立ては要さない。**
// `movements.mjs` は直接実行されたときだけ走査を始めるため、ここで読み込んでも
// ホストへは触れない。

import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  WRITE,
  buildDocument,
  lostMovement,
  observedHasMovement,
  tableIsEmpty,
  verdict,
} from "./movements.mjs";

/** 移動を失った観測。 */
const LOST = { outcome: WRITE.notApplied, observed: "0.00" };

/** 移動が残った観測。 */
const KEPT = { outcome: WRITE.notApplied, observed: "0.00,100.00,直線移動,0|15" };

describe("observedHasMovement", () => {
  it("reads a value without a separator as having lost its movement", () => {
    // 移動を持たない値は区間の数に依らず単一の数値になる。
    assert.equal(observedHasMovement("0.00"), false);
    assert.equal(observedHasMovement("-600.00"), false);
    assert.equal(observedHasMovement("100"), false);
  });

  it("reads a value with a movement as keeping it", () => {
    assert.equal(observedHasMovement("0.00,100.00,直線移動,0"), true);
    // パラメータを持つ移動方法も移動である。
    assert.equal(observedHasMovement("0.00,100.00,直線移動,0|15"), true);
  });

  it("does not call an unreadable value a lost movement", () => {
    // 「書けない」と名乗るには移動が失われたという観測が要る。読めない表記は
    // その観測にならない。
    assert.equal(observedHasMovement(null), true);
    assert.equal(observedHasMovement(""), true);
    assert.equal(observedHasMovement("読めない表記"), true);
  });
});

describe("lostMovement", () => {
  it("counts only a write that came back without its movement", () => {
    assert.equal(lostMovement(LOST), true);
    assert.equal(lostMovement(KEPT), false);
    assert.equal(lostMovement({ outcome: WRITE.accepted }), false);
    assert.equal(lostMovement({ outcome: WRITE.failed, reason: "track_value_count" }), false);
    assert.equal(lostMovement({ outcome: WRITE.unprepared, reason: "巻き戻せない" }), false);
    assert.equal(lostMovement({ outcome: WRITE.raised, reason: "selector に追従できません" }), false);
  });
});

describe("verdict", () => {
  it("records an accepted write as writable without a second look", () => {
    // 書き戻し照合が値・移動方法・フラグの構造一致を要求するため、受理された
    // 側は補強を要さない。
    assert.deepEqual(verdict({ outcome: WRITE.accepted }), { recorded: true, writable: true });
  });

  it("leaves a surviving movement off the table", () => {
    // 違うのは値かパラメータであって移動の有無ではない。
    const decision = verdict(KEPT);
    assert.equal(decision.recorded, false);
    assert.match(decision.reason, /0\.00,100\.00,直線移動,0\|15/);
  });

  it("leaves a name the deployed table already refuses off the table", () => {
    // 表が既に埋まっていれば、走査は前回の出力を写すだけになる。
    const decision = verdict({ outcome: WRITE.failed, reason: "track_mode_not_writable" });
    assert.equal(decision.recorded, false);
    assert.match(decision.reason, /track_mode_not_writable/);
    assert.match(decision.reason, /表が古い可能性/);
  });

  it("leaves any other failure off the table with its reason", () => {
    const decision = verdict({ outcome: WRITE.failed, reason: "track_value_count" });
    assert.equal(decision.recorded, false);
    assert.match(decision.reason, /track_value_count/);
  });

  it("never spells a failure that named no reason as undefined", () => {
    for (const result of [
      { outcome: WRITE.failed },
      { outcome: WRITE.failed, reason: "" },
      { outcome: WRITE.unprepared },
      { outcome: WRITE.raised },
    ]) {
      const decision = verdict(result);
      assert.equal(decision.recorded, false);
      assert.doesNotMatch(decision.reason, /undefined|null/);
      assert.notEqual(decision.reason, "");
    }
  });

  it("leaves a measurement that could not be set up off the table", () => {
    const decision = verdict({ outcome: WRITE.unprepared, reason: "item_value_not_applied: 0.00" });
    assert.equal(decision.recorded, false);
    assert.match(decision.reason, /土台/);
    assert.match(decision.reason, /item_value_not_applied: 0\.00/);
  });

  it("leaves a measurement that raised off the table", () => {
    // 1 件の失敗で走査を止めないため、例外も結末として畳まれてここへ来る。
    const decision = verdict({ outcome: WRITE.raised, reason: "selector に追従できません" });
    assert.equal(decision.recorded, false);
    assert.match(decision.reason, /例外/);
    assert.match(decision.reason, /selector に追従できません/);
  });

  it("does not record a lost movement seen only once", () => {
    // 焼き込まれた偽はその名前を全オブジェクトで拒む。1 回の観測では確定しない。
    const decision = verdict(LOST);
    assert.equal(decision.recorded, false);
    assert.match(decision.reason, /確かめ直していない/);
  });

  it("records a movement lost at both section counts as not writable", () => {
    assert.deepEqual(verdict(LOST, LOST), { recorded: true, writable: false });
  });

  it("leaves a name whose result changed with the section count off the table", () => {
    // 区間の境界より多くの制御点を要する移動方法がここへ来る。名前について
    // 全域で成り立つ性質ではないため、表に書けない。
    for (const second of [{ outcome: WRITE.accepted }, KEPT]) {
      const decision = verdict(LOST, second);
      assert.equal(decision.recorded, false);
      assert.match(decision.reason, /区間/);
    }
  });

  it("leaves a name whose second look could not be measured off the table", () => {
    for (const second of [
      { outcome: WRITE.failed, reason: "track_value_count" },
      { outcome: WRITE.unprepared, reason: "section_boundary_exists" },
      { outcome: WRITE.raised, reason: "オブジェクトを読めません" },
    ]) {
      const decision = verdict(LOST, second);
      assert.equal(decision.recorded, false);
      assert.match(decision.reason, /確かめ直し/);
    }
  });
});

describe("buildDocument", () => {
  /** 走査 1 件分の測定結果を組み立てる。 */
  const measured = (name, first, second = null) => ({ name, verdict: verdict(first, second) });

  it("sorts the names it records", () => {
    const table = buildDocument([
      measured("直線移動", { outcome: WRITE.accepted }),
      measured("回転", { outcome: WRITE.accepted }),
      measured("移動無し", LOST, LOST),
    ]);
    assert.deepEqual(Object.keys(table.movements), ["回転", "直線移動", "移動無し"]);
    assert.deepEqual(table.movements, {
      回転: { writable: true },
      移動無し: { writable: false },
      直線移動: { writable: true },
    });
  });

  it("keeps the names it could not judge out of the table", () => {
    // 「測っていない」を表す形は表に無い。載せない名前は現れてはならない。
    const table = buildDocument([
      measured("直線移動", { outcome: WRITE.accepted }),
      measured("測れない移動", { outcome: WRITE.failed, reason: "track_mode_not_writable" }),
      measured("値だけ違う移動", KEPT),
      measured("落ちた移動", { outcome: WRITE.raised, reason: "追従できません" }),
      measured("区間で変わる移動", LOST, { outcome: WRITE.accepted }),
      measured("1 度しか見ていない移動", LOST),
    ]);
    assert.deepEqual(Object.keys(table.movements), ["直線移動"]);
    for (const name of [
      "測れない移動",
      "値だけ違う移動",
      "落ちた移動",
      "区間で変わる移動",
      "1 度しか見ていない移動",
    ]) {
      assert.equal(JSON.stringify(table).includes(name), false, name);
    }
  });

  it("writes the verdict as a boolean", () => {
    // 読む側は真偽値以外を拒む。`null` も省略も受け付けない。
    const table = buildDocument([
      measured("直線移動", { outcome: WRITE.accepted }),
      measured("移動無し", LOST, LOST),
    ]);
    for (const facet of Object.values(table.movements)) {
      assert.equal(typeof facet.writable, "boolean");
      assert.deepEqual(Object.keys(facet), ["writable"]);
    }
  });

  it("always carries the movements key", () => {
    assert.deepEqual(buildDocument([]), { movements: {} });
  });
});

describe("tableIsEmpty", () => {
  it("calls a table without entries empty", () => {
    assert.equal(tableIsEmpty('{\n  "movements": {}\n}\n'), true);
  });

  it("calls a table with a single entry not empty", () => {
    assert.equal(tableIsEmpty('{"movements":{"移動無し":{"writable":false}}}'), false);
  });

  it("does not call a table it cannot read empty", () => {
    // 直し方は空でない表と同じである——空へ戻してから走らせ直す。
    assert.equal(tableIsEmpty("{"), false);
    assert.equal(tableIsEmpty('{"movement":{}}'), false);
    assert.equal(tableIsEmpty('{"movements":null}'), false);
    assert.equal(tableIsEmpty('{"movements":[]}'), false);
  });
});
