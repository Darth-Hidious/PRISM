import { ChildProcessWithoutNullStreams } from "child_process";
import * as readline from "readline";
import * as vscode from "vscode";
import {
  JsonRpcId,
  JsonRpcMessage,
  JsonRpcNotification,
  JsonRpcResponse,
} from "../protocol/types";

interface PendingRequest {
  method: string;
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
}

function isResponse(message: JsonRpcMessage): message is JsonRpcResponse {
  return "id" in message && ("result" in message || "error" in message);
}

function isNotification(message: JsonRpcMessage): message is JsonRpcNotification {
  return "method" in message && !("id" in message);
}

export class JsonRpcStdioClient implements vscode.Disposable {
  private nextId = 1;
  private process: ChildProcessWithoutNullStreams | undefined;
  private readonly pending = new Map<JsonRpcId, PendingRequest>();
  private readonly notificationEmitter =
    new vscode.EventEmitter<JsonRpcNotification>();
  private readonly closeEmitter = new vscode.EventEmitter<number | null>();

  readonly onNotification = this.notificationEmitter.event;
  readonly onClose = this.closeEmitter.event;

  attach(child: ChildProcessWithoutNullStreams): void {
    this.disposeProcess();
    this.process = child;

    const rl = readline.createInterface({ input: child.stdout });
    rl.on("line", (line) => this.handleLine(line));
    child.once("exit", (code) => {
      rl.close();
      this.rejectAll(new Error(`PRISM backend exited with code ${code ?? "null"}`));
      this.closeEmitter.fire(code);
      this.process = undefined;
    });
  }

  async sendRequest<T>(method: string, params?: unknown): Promise<T> {
    const child = this.requireProcess();
    const id = this.nextId++;
    const payload = {
      jsonrpc: "2.0",
      id,
      method,
      params,
    };

    const result = new Promise<T>((resolve, reject) => {
      this.pending.set(id, {
        method,
        resolve: (value) => resolve(value as T),
        reject,
      });
    });

    child.stdin.write(`${JSON.stringify(payload)}\n`);
    return result;
  }

  sendNotification(method: string, params?: unknown): void {
    const child = this.requireProcess();
    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`);
  }

  stop(): void {
    this.disposeProcess();
  }

  dispose(): void {
    this.disposeProcess();
    this.notificationEmitter.dispose();
    this.closeEmitter.dispose();
  }

  private handleLine(line: string): void {
    if (!line.trim()) {
      return;
    }

    let message: JsonRpcMessage;
    try {
      message = JSON.parse(line) as JsonRpcMessage;
    } catch (error) {
      this.notificationEmitter.fire({
        jsonrpc: "2.0",
        method: "prism.protocol.invalidJson",
        params: { line, error: String(error) },
      });
      return;
    }

    if (isResponse(message)) {
      const pending = this.pending.get(message.id);
      if (!pending) {
        return;
      }
      this.pending.delete(message.id);
      if (message.error) {
        pending.reject(
          new Error(`${pending.method} failed: ${message.error.message}`)
        );
      } else {
        pending.resolve(message.result);
      }
      return;
    }

    if (isNotification(message)) {
      this.notificationEmitter.fire(message);
    }
  }

  private requireProcess(): ChildProcessWithoutNullStreams {
    if (!this.process || this.process.killed) {
      throw new Error("PRISM backend is not running.");
    }
    return this.process;
  }

  private disposeProcess(): void {
    if (this.process && !this.process.killed) {
      this.process.kill();
    }
    this.process = undefined;
    this.rejectAll(new Error("PRISM backend stopped."));
  }

  private rejectAll(error: Error): void {
    for (const pending of this.pending.values()) {
      pending.reject(error);
    }
    this.pending.clear();
  }
}
