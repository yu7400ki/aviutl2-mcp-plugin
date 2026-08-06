// AviUtl2 MCP サーバーを stdio で駆動する最小のクライアント。
//
// 生成器はこのディレクトリだけで完結させる。製品のクレートを参照しないため、
// ワークスペースのビルドにも lint にも関与しない。

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

/** リポジトリのルート。 */
export const REPOSITORY = join(HERE, "..", "..");

/** 要求 1 件の待ち時間の上限（ミリ秒）。 */
const REQUEST_TIMEOUT_MS = 60000;

/**
 * サーバーの実行ファイルを探す。
 *
 * release を先に見るのは、そちらが在るなら配布に近い方で確かめたいためである。
 */
export function resolveServerPath() {
  for (const profile of ["release", "debug"]) {
    const path = join(REPOSITORY, "target", profile, "aviutl2-mcp-server.exe");
    if (existsSync(path)) return path;
  }
  throw new Error(
    "aviutl2-mcp-server.exe が target/release にも target/debug にも見つかりません。--server で指定してください",
  );
}

/** JSON-RPC を 1 行 1 メッセージで往復する stdio クライアント。 */
export class Mcp {
  constructor(serverPath = resolveServerPath()) {
    this.proc = spawn(serverPath, [], { stdio: ["pipe", "pipe", "pipe"] });
    this.nextId = 1;
    this.pending = new Map();
    this.buffer = "";
    this.stderr = "";
    this.proc.stdout.setEncoding("utf8");
    this.proc.stdout.on("data", (chunk) => this.#consume(chunk));
    this.proc.stderr.setEncoding("utf8");
    this.proc.stderr.on("data", (chunk) => {
      this.stderr += chunk;
    });
  }

  #consume(chunk) {
    this.buffer += chunk;
    let end;
    while ((end = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, end).trim();
      this.buffer = this.buffer.slice(end + 1);
      if (!line) continue;
      let message;
      try {
        message = JSON.parse(line);
      } catch {
        continue;
      }
      const waiter = this.pending.get(message.id);
      if (!waiter) continue;
      this.pending.delete(message.id);
      if (message.error) waiter.reject(new Error(JSON.stringify(message.error)));
      else waiter.resolve(message.result);
    }
  }

  request(method, params) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
      setTimeout(() => {
        if (!this.pending.delete(id)) return;
        reject(new Error(`要求が時間内に返りませんでした: ${method}`));
      }, REQUEST_TIMEOUT_MS).unref();
    });
  }

  async init() {
    const result = await this.request("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "effect-choices-generator", version: "1" },
    });
    this.proc.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" })}\n`);
    return result;
  }

  /** tool を 1 件呼び、構造化出力と失敗の別を返す。 */
  async call(name, args) {
    const result = await this.request("tools/call", { name, arguments: args });
    return {
      isError: result.isError === true,
      data: result.structuredContent,
      text: (result.content ?? []).map((part) => part.text).join("\n"),
    };
  }

  close() {
    this.proc.stdin.end();
    this.proc.kill();
  }
}
