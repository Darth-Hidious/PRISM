import { spawn } from "child_process";
import * as path from "path";
import * as vscode from "vscode";
import { JsonRpcStdioClient } from "./jsonRpc";
import { BackendNotification, OkResult } from "../protocol/types";

export type BackendState = "stopped" | "starting" | "running";

export class PrismBackend implements vscode.Disposable {
  private readonly rpc = new JsonRpcStdioClient();
  private state: BackendState = "stopped";
  private initialized = false;
  private readonly stateEmitter = new vscode.EventEmitter<BackendState>();
  private readonly notificationEmitter =
    new vscode.EventEmitter<BackendNotification>();

  readonly onStateChanged = this.stateEmitter.event;
  readonly onNotification = this.notificationEmitter.event;

  constructor(private readonly output: vscode.OutputChannel) {
    this.rpc.onNotification((notification) => {
      this.notificationEmitter.fire({
        method: notification.method,
        params: notification.params,
        receivedAt: new Date().toISOString(),
      });
    });
    this.rpc.onClose(() => {
      this.initialized = false;
      this.setState("stopped");
    });
  }

  get currentState(): BackendState {
    return this.state;
  }

  async start(): Promise<void> {
    if (this.state === "running" || this.state === "starting") {
      return;
    }

    this.setState("starting");
    const config = vscode.workspace.getConfiguration("prism");
    const command = config.get<string>("backendCommand", "prism");
    const backendArgs = config.get<string[]>("backendArgs", ["backend"]);
    const pythonPath = config.get<string>("pythonPath", "python3");
    const projectRoot = this.projectRoot();
    const args = [
      ...backendArgs,
      "--project-root",
      projectRoot,
      "--python",
      pythonPath,
    ];

    this.output.appendLine(`Starting PRISM backend: ${command} ${args.join(" ")}`);
    const child = spawn(command, args, {
      cwd: projectRoot,
      env: {
        ...process.env,
        PRISM_FRONTEND: "vscode",
      },
      stdio: "pipe",
    });

    child.stderr.on("data", (chunk: Buffer) => {
      this.output.append(chunk.toString());
    });
    child.once("error", (error) => {
      this.output.appendLine(`Failed to start PRISM backend: ${error.message}`);
      this.initialized = false;
      this.setState("stopped");
    });

    this.rpc.attach(child);
    this.setState("running");
    await this.initialize();
  }

  async stop(): Promise<void> {
    this.initialized = false;
    this.rpc.stop();
    this.setState("stopped");
  }

  async ensureStarted(): Promise<void> {
    const config = vscode.workspace.getConfiguration("prism");
    const autoStart = config.get<boolean>("autoStartBackend", true);
    if (this.state === "running") {
      return;
    }
    if (!autoStart) {
      throw new Error("PRISM backend is not running.");
    }
    await this.start();
  }

  async sendMessage(text: string): Promise<OkResult> {
    await this.ensureStarted();
    return this.rpc.sendRequest<OkResult>("input.message", { text });
  }

  async sendCommand(command: string, silent = false): Promise<OkResult> {
    await this.ensureStarted();
    return this.rpc.sendRequest<OkResult>("input.command", { command, silent });
  }

  async respondToApproval(response: "y" | "n" | "a" | "b"): Promise<OkResult> {
    await this.ensureStarted();
    return this.rpc.sendRequest<OkResult>("approval.respond", { response });
  }

  dispose(): void {
    this.rpc.dispose();
    this.stateEmitter.dispose();
    this.notificationEmitter.dispose();
  }

  private async initialize(): Promise<void> {
    if (this.initialized) {
      return;
    }
    await this.rpc.sendRequest<OkResult>("init", {
      auto_approve: false,
      resume: "",
    });
    this.initialized = true;
  }

  private projectRoot(): string {
    const folder = vscode.workspace.workspaceFolders?.[0];
    return folder?.uri.fsPath ?? path.resolve(process.cwd());
  }

  private setState(next: BackendState): void {
    if (this.state === next) {
      return;
    }
    this.state = next;
    this.stateEmitter.fire(next);
  }
}
