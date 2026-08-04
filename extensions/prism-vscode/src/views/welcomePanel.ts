import * as vscode from "vscode";
import { nonceValue } from "./chatHtml";

export interface WelcomeActions {
  sendMessage: (text: string) => Promise<void>;
}

/**
 * Welcome tab carried over from the legacy prism-welcome extension, made
 * offline-safe: the clock, greeting, and quick actions stay; the NASA/ESA
 * image fetches are gone (repo-wide offline policy), and the webview can
 * only trigger whitelisted prism.* commands — never arbitrary ones.
 */
export class WelcomePanel {
  static readonly viewType = "prism.welcome";
  private static current: WelcomePanel | undefined;

  static createOrShow(
    extensionUri: vscode.Uri,
    actions: WelcomeActions
  ): void {
    if (WelcomePanel.current) {
      WelcomePanel.current.panel.reveal(vscode.ViewColumn.One);
      return;
    }
    const panel = vscode.window.createWebviewPanel(
      WelcomePanel.viewType,
      "Welcome to PRISM",
      vscode.ViewColumn.One,
      { enableScripts: true, retainContextWhenHidden: true }
    );
    WelcomePanel.current = new WelcomePanel(panel, extensionUri, actions);
  }

  private constructor(
    private readonly panel: vscode.WebviewPanel,
    extensionUri: vscode.Uri,
    private readonly actions: WelcomeActions
  ) {
    panel.iconPath = vscode.Uri.joinPath(extensionUri, "resources", "prism.svg");
    panel.webview.html = this.html(panel.webview);
    panel.webview.onDidReceiveMessage((message: unknown) => {
      void this.handleMessage(message);
    });
    panel.onDidDispose(() => {
      WelcomePanel.current = undefined;
    });
  }

  private async handleMessage(message: unknown): Promise<void> {
    if (typeof message !== "object" || message === null) {
      return;
    }
    const payload = message as { type?: string; action?: string; text?: string };
    if (payload.type === "action" && payload.action) {
      // Whitelist only — the legacy panel executed whatever the webview sent.
      if (ALLOWED_ACTIONS.includes(payload.action)) {
        await vscode.commands.executeCommand(payload.action);
      }
      return;
    }
    if (payload.type === "ask" && payload.text?.trim()) {
      try {
        await this.actions.sendMessage(payload.text.trim());
        await vscode.commands.executeCommand("prism.openAgent");
      } catch (error) {
        await vscode.window.showErrorMessage(String(error));
      }
    }
  }

  private html(webview: vscode.Webview): string {
    const nonce = nonceValue();
    const actionsJson = JSON.stringify(QUICK_ACTIONS);
    const quotesJson = JSON.stringify(QUOTES);
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'nonce-${nonce}'; script-src 'nonce-${nonce}'; img-src ${webview.cspSource};">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <style nonce="${nonce}">
    * { margin: 0; padding: 0; box-sizing: border-box; }
    body {
      width: 100vw; height: 100vh; overflow: hidden;
      font-family: var(--vscode-font-family);
      color: #f4f4f8; user-select: none;
      background:
        radial-gradient(1px 1px at 20% 30%, #fff 50%, transparent 51%),
        radial-gradient(1px 1px at 70% 12%, #ffe9c4 50%, transparent 51%),
        radial-gradient(1.5px 1.5px at 45% 68%, #cfe3ff 50%, transparent 51%),
        radial-gradient(1px 1px at 85% 55%, #fff 50%, transparent 51%),
        radial-gradient(1px 1px at 10% 80%, #ffd7e0 50%, transparent 51%),
        radial-gradient(1.5px 1.5px at 60% 40%, #fff 50%, transparent 51%),
        radial-gradient(ellipse at 50% 120%, #2b2e59 0%, transparent 60%),
        linear-gradient(180deg, #0d0e15 0%, #141826 60%, #1a1f33 100%);
    }
    #top-bar { position: fixed; top: 0; left: 0; right: 0; padding: 26px 40px; text-align: center; }
    #greeting { font-size: 15px; font-weight: 300; opacity: 0.85; text-shadow: 0 1px 8px rgba(0,0,0,0.6); }
    #center { position: fixed; top: 38%; left: 50%; transform: translate(-50%, -50%); text-align: center; }
    #clock { font-size: 110px; font-weight: 200; letter-spacing: -4px; line-height: 1; text-shadow: 0 2px 20px rgba(0,0,0,0.5); }
    #date-line { font-size: 16px; font-weight: 300; margin-top: 8px; opacity: 0.7; }
    #actions {
      position: fixed; bottom: 110px; left: 50%; transform: translateX(-50%);
      display: flex; gap: 12px; flex-wrap: wrap; justify-content: center;
    }
    #actions button {
      padding: 9px 16px; border-radius: 10px; cursor: pointer; font-size: 13px;
      color: #f4f4f8; background: rgba(255,255,255,0.1);
      border: 1px solid rgba(255,255,255,0.18);
      backdrop-filter: blur(10px);
    }
    #actions button:hover { background: rgba(255,255,255,0.2); }
    #ask-bar {
      position: fixed; bottom: 36px; left: 50%; transform: translateX(-50%);
      display: flex; gap: 8px; width: 560px; max-width: 90vw;
    }
    #ask-input {
      flex: 1; resize: none; padding: 10px 14px; border-radius: 12px;
      color: #fff; background: rgba(255,255,255,0.08);
      border: 1px solid rgba(255,255,255,0.15); outline: none; font: inherit;
    }
    #ask-input::placeholder { color: rgba(255,255,255,0.35); }
    #ask-send {
      width: 42px; border-radius: 12px; cursor: pointer; color: #fbbf24;
      background: rgba(251,191,36,0.2); border: 1px solid rgba(251,191,36,0.3);
    }
    @media (max-width: 700px) { #clock { font-size: 64px; } }
  </style>
</head>
<body>
  <div id="top-bar"><span id="greeting"></span></div>
  <div id="center">
    <div id="clock"></div>
    <div id="date-line"></div>
  </div>
  <div id="actions"></div>
  <div id="ask-bar">
    <textarea id="ask-input" rows="1" placeholder="Ask the PRISM agent..."></textarea>
    <button id="ask-send" title="Send">&#9654;</button>
  </div>
  <script nonce="${nonce}">
    const vscodeApi = acquireVsCodeApi();
    const ACTIONS = ${actionsJson};
    const QUOTES = ${quotesJson};

    function updateClock() {
      const now = new Date();
      document.getElementById("clock").textContent =
        String(now.getHours()).padStart(2, "0") + ":" + String(now.getMinutes()).padStart(2, "0");
      document.getElementById("date-line").textContent =
        now.toLocaleDateString(undefined, { weekday: "long", year: "numeric", month: "long", day: "numeric" });
    }
    updateClock();
    setInterval(updateClock, 10000);

    document.getElementById("greeting").textContent =
      QUOTES[Math.floor(Math.random() * QUOTES.length)];

    const actionsEl = document.getElementById("actions");
    for (const action of ACTIONS) {
      const button = document.createElement("button");
      button.textContent = action.label;
      button.addEventListener("click", () => {
        vscodeApi.postMessage({ type: "action", action: action.command });
      });
      actionsEl.appendChild(button);
    }

    const input = document.getElementById("ask-input");
    function send() {
      const text = input.value.trim();
      if (!text) return;
      vscodeApi.postMessage({ type: "ask", text });
      input.value = "";
    }
    document.getElementById("ask-send").addEventListener("click", send);
    input.addEventListener("keydown", (event) => {
      if (event.key === "Enter" && !event.shiftKey) {
        event.preventDefault();
        send();
      }
    });
  </script>
</body>
</html>`;
  }
}

const ALLOWED_ACTIONS: readonly string[] = [
  "prism.openAgent",
  "prism.openChatTab",
  "prism.startBackend",
  "prism.openPeriodicTable",
  "prism.marketplaceSearch",
  "prism.listNodes",
  "prism.accountStatus",
];

const QUICK_ACTIONS = [
  { label: "Open Agent", command: "prism.openAgent" },
  { label: "Chat Tab", command: "prism.openChatTab" },
  { label: "Start Backend", command: "prism.startBackend" },
  { label: "Periodic Table", command: "prism.openPeriodicTable" },
  { label: "Marketplace", command: "prism.marketplaceSearch" },
  { label: "Nodes", command: "prism.listNodes" },
];

const QUOTES = [
  "Somewhere, something incredible is waiting to be known.",
  "The atoms of our bodies are traceable to stars that manufactured them.",
  "We are a way for the universe to know itself.",
  "Materials are the language of engineering.",
  "The next breakthrough is hidden in the periodic table.",
  "Look up at the stars and not down at your feet.",
  "Every atom in your body came from a star that exploded.",
];
