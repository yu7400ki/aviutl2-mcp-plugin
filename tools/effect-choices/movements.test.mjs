// 移動方法の可否の生成器のうち、実機を要さない部分を確かめる。
//
// 走査そのものは起動中の AviUtl2 を要するが、**判定と組み立ては要さない。**
// `movements.mjs` は直接実行されたときだけ走査を始めるため、ここで読み込んでも
// ホストへは触れない。

import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { WRITE, buildDocument, observedHasMovement, tableIsEmpty, verdict } from "./movements.mjs";

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

describe("verdict", () => {
  it("records an accepted write as writable", () => {
    assert.deepEqual(verdict({ outcome: WRITE.accepted }), { recorded: true, writable: true });
  });

  it("records a lost movement as not writable", () => {
    assert.deepEqual(verdict({ outcome: WRITE.notApplied, observed: "0.00" }), {
      recorded: true,
      writable: false,
    });
  });

  it("leaves a surviving movement off the table", () => {
    // 違うのは値かパラメータであって移動の有無ではない。
    const decision = verdict({ outcome: WRITE.notApplied, observed: "0.00,100.00,直線移動,0|15" });
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
});

describe("buildDocument", () => {
  /** 走査 1 件分の測定結果を組み立てる。 */
  const measured = (name, result) => ({ name, verdict: verdict(result) });

  it("sorts the names it records", () => {
    const table = buildDocument([
      measured("直線移動", { outcome: WRITE.accepted }),
      measured("回転", { outcome: WRITE.accepted }),
      measured("移動無し", { outcome: WRITE.notApplied, observed: "0.00" }),
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
      measured("値だけ違う移動", { outcome: WRITE.notApplied, observed: "0.00,100.00,回転,0" }),
    ]);
    assert.deepEqual(Object.keys(table.movements), ["直線移動"]);
    assert.equal(JSON.stringify(table).includes("測れない移動"), false);
    assert.equal(JSON.stringify(table).includes("値だけ違う移動"), false);
  });

  it("writes the verdict as a boolean", () => {
    // 読む側は真偽値以外を拒む。`null` も省略も受け付けない。
    const table = buildDocument([
      measured("直線移動", { outcome: WRITE.accepted }),
      measured("移動無し", { outcome: WRITE.notApplied, observed: "0.00" }),
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
