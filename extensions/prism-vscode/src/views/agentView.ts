import * as vscode from "vscode";
import { BackendNotification } from "../protocol/types";

export interface AgentViewActions {
  startBackend: () => Promise<void>;
  sendMessage: (text: string) => Promise<void>;
  sendCommand: (command: string) => Promise<void>;
  approve: (response: "y" | "n" | "a" | "b") => Promise<void>;
}

export class AgentViewProvider implements vscode.WebviewViewProvider {
  private view: vscode.WebviewView | undefined;
  private readonly events: BackendNotification[] = [];

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly actions: AgentViewActions
  ) {}

  resolveWebviewView(webviewView: vscode.WebviewView): void {
    this.view = webviewView;
    webviewView.webview.options = {
      enableScripts: true,
      localResourceRoots: [this.extensionUri],
    };
    webviewView.webview.html = this.html(webviewView.webview);
    webviewView.webview.onDidReceiveMessage((message: unknown) => {
      void this.handleMessage(message);
    });
  }

  append(notification: BackendNotification): void {
    this.events.push(notification);
    if (this.events.length > 120) {
      this.events.shift();
    }
    void this.view?.webview.postMessage({
      type: "notification",
      notification,
    });
  }

  postStatus(status: string): void {
    void this.view?.webview.postMessage({ type: "status", status });
  }

  private async handleMessage(message: unknown): Promise<void> {
    if (typeof message !== "object" || message === null) {
      return;
    }
    const payload = message as { type?: string; text?: string; response?: string };
    try {
      if (payload.type === "start") {
        await this.actions.startBackend();
      } else if (payload.type === "send" && payload.text?.trim()) {
        const text = payload.text.trim();
        if (text.startsWith("/")) {
          await this.actions.sendCommand(text);
        } else {
          await this.actions.sendMessage(text);
        }
      } else if (
        payload.type === "approve" &&
        (payload.response === "y" ||
          payload.response === "n" ||
          payload.response === "a" ||
          payload.response === "b")
      ) {
        await this.actions.approve(payload.response);
      }
    } catch (error) {
      await vscode.window.showErrorMessage(String(error));
    }
  }

  private html(webview: vscode.Webview): string {
    const nonce = nonceValue();
    const initialEvents = JSON.stringify(this.events);
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}';">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <style>
    :root {
      color-scheme: light dark;
      --border: color-mix(in srgb, var(--vscode-foreground) 16%, transparent);
      --muted: color-mix(in srgb, var(--vscode-foreground) 64%, transparent);
    }
    body {
      padding: 0;
      margin: 0;
      color: var(--vscode-foreground);
      background: var(--vscode-sideBar-background);
      font: var(--vscode-font-size) var(--vscode-font-family);
    }
    .shell {
      display: grid;
      grid-template-rows: auto 1fr auto;
      min-height: 100vh;
    }
    header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 8px;
      padding: 10px 12px;
      border-bottom: 1px solid var(--border);
    }
    .title {
      font-weight: 650;
    }
    .status {
      color: var(--muted);
      font-size: 12px;
      white-space: nowrap;
    }
    button {
      border: 1px solid var(--vscode-button-border, transparent);
      border-radius: 4px;
      color: var(--vscode-button-foreground);
      background: var(--vscode-button-background);
      padding: 5px 9px;
      font: inherit;
      cursor: pointer;
    }
    button:hover {
      background: var(--vscode-button-hoverBackground);
    }
    main {
      overflow: auto;
      padding: 10px 12px;
    }
    .event {
      border: 1px solid var(--border);
      border-radius: 6px;
      padding: 8px;
      margin-bottom: 8px;
      background: color-mix(in srgb, var(--vscode-sideBar-background) 84%, var(--vscode-editor-background));
    }
    .event-method {
      font-weight: 650;
      margin-bottom: 4px;
    }
    .event-time {
      color: var(--muted);
      font-size: 11px;
      margin-left: 6px;
    }
    pre {
      white-space: pre-wrap;
      word-break: break-word;
      margin: 0;
      color: var(--muted);
    }
    form {
      display: grid;
      gap: 8px;
      padding: 10px 12px;
      border-top: 1px solid var(--border);
      background: var(--vscode-sideBar-background);
    }
    textarea {
      box-sizing: border-box;
      width: 100%;
      min-height: 72px;
      resize: vertical;
      border: 1px solid var(--vscode-input-border, var(--border));
      border-radius: 4px;
      color: var(--vscode-input-foreground);
      background: var(--vscode-input-background);
      padding: 8px;
      font: inherit;
    }
    .actions {
      display: flex;
      gap: 8px;
      flex-wrap: wrap;
    }
    .empty {
      color: var(--muted);
      padding: 18px 0;
    }
  </style>
</head>
<body>
  <div class="shell">
    <header>
      <div>
        <div class="title">PRISM Agent</div>
        <div class="status" id="status">Backend stopped</div>
      </div>
      <button id="start" title="Start local PRISM backend">Start</button>
    </header>
    <main id="events"></main>
    <form id="composer">
      <textarea id="input" placeholder="Ask PRISM, or run a slash command like /models list"></textarea>
      <div class="actions">
        <button type="submit">Send</button>
        <button type="button" data-approval="y">Allow Once</button>
        <button type="button" data-approval="n">Deny</button>
        <button type="button" data-approval="a">Allow Session</button>
      </div>
    </form>
  </div>
  <script nonce="${nonce}">
    const vscode = acquireVsCodeApi();
    const events = ${initialEvents};
    const eventsEl = document.getElementById("events");
    const statusEl = document.getElementById("status");
    const inputEl = document.getElementById("input");

    function renderEvent(event) {
      const node = document.createElement("section");
      node.className = "event";
      const title = document.createElement("div");
      title.className = "event-method";
      title.textContent = event.method || "event";
      const time = document.createElement("span");
      time.className = "event-time";
      time.textContent = event.receivedAt ? new Date(event.receivedAt).toLocaleTimeString() : "";
      title.appendChild(time);
      const pre = document.createElement("pre");
      pre.textContent = event.params === undefined ? "" : JSON.stringify(event.params, null, 2);
      node.appendChild(title);
      node.appendChild(pre);
      return node;
    }

    function render() {
      eventsEl.innerHTML = "";
      if (!events.length) {
        const empty = document.createElement("div");
        empty.className = "empty";
        empty.textContent = "Start the backend or send a message to begin.";
        eventsEl.appendChild(empty);
        return;
      }
      for (const event of events) {
        eventsEl.appendChild(renderEvent(event));
      }
      eventsEl.scrollTop = eventsEl.scrollHeight;
    }

    document.getElementById("start").addEventListener("click", () => {
      vscode.postMessage({ type: "start" });
    });
    document.getElementById("composer").addEventListener("submit", (event) => {
      event.preventDefault();
      const text = inputEl.value.trim();
      if (!text) return;
      vscode.postMessage({ type: "send", text });
      inputEl.value = "";
    });
    for (const button of document.querySelectorAll("[data-approval]")) {
      button.addEventListener("click", () => {
        vscode.postMessage({ type: "approve", response: button.dataset.approval });
      });
    }
    window.addEventListener("message", (event) => {
      const message = event.data;
      if (message.type === "notification") {
        events.push(message.notification);
        while (events.length > 120) events.shift();
        render();
      } else if (message.type === "status") {
        statusEl.textContent = message.status;
      }
    });
    render();
  </script>
</body>
</html>`;
  }
}

function nonceValue(): string {
  const alphabet =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  let value = "";
  for (let i = 0; i < 32; i++) {
    value += alphabet.charAt(Math.floor(Math.random() * alphabet.length));
  }
  return value;
}
