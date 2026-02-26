// frontend/src/bridge/client.ts — JSON-RPC 2.0 client over stdio
import { spawn, type ChildProcess } from "child_process";
import { createInterface, type Interface } from "readline";
import { EventEmitter } from "events";

export interface JsonRpcMessage {
  jsonrpc: "2.0";
  method?: string;
  params?: Record<string, unknown>;
  id?: number;
  result?: unknown;
  error?: { code: number; message: string };
}

export class BackendClient extends EventEmitter {
  private process: ChildProcess;
  private rl: Interface;
  private nextId = 1;
  private pending = new Map<
    number,
    { resolve: (value: unknown) => void; reject: (reason: Error) => void }
  >();

  constructor(pythonPath: string) {
    super();
    this.process = spawn(pythonPath, ["-m", "app.backend"], {
      stdio: ["pipe", "pipe", "inherit"],
    });

    this.rl = createInterface({ input: this.process.stdout! });
    this.rl.on("line", (line) => this.handleLine(line));

    this.process.on("exit", (code) => {
      this.emit("exit", code);
    });
  }

  private handleLine(line: string) {
    try {
      const msg: JsonRpcMessage = JSON.parse(line);

      // Response to a request
      if (
        msg.id !== undefined &&
        (msg.result !== undefined || msg.error !== undefined)
      ) {
        const pending = this.pending.get(msg.id);
        if (pending) {
          this.pending.delete(msg.id);
          if (msg.error) pending.reject(new Error(msg.error.message));
          else pending.resolve(msg.result);
        }
        return;
      }

      // Notification (backend -> frontend event)
      if (msg.method) {
        this.emit("event", { method: msg.method, params: msg.params || {} });
      }
    } catch {
      // Ignore unparseable lines (e.g. Python tracebacks on stderr leak)
    }
  }

  async request(
    method: string,
    params: Record<string, unknown> = {},
  ): Promise<unknown> {
    const id = this.nextId++;
    const msg = JSON.stringify({ jsonrpc: "2.0", method, params, id });
    this.process.stdin!.write(msg + "\n");

    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      setTimeout(() => {
        if (this.pending.has(id)) {
          this.pending.delete(id);
          reject(new Error(`Request ${method} timed out`));
        }
      }, 30_000);
    });
  }

  /** Fire-and-forget: send a JSON-RPC request without waiting for response. */
  send(method: string, params: Record<string, unknown> = {}) {
    const id = this.nextId++;
    const msg = JSON.stringify({ jsonrpc: "2.0", method, params, id });
    this.process.stdin!.write(msg + "\n");
  }

  destroy() {
    this.process.kill();
  }
}
