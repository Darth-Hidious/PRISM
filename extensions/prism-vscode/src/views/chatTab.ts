import * as vscode from "vscode";
import { BackendNotification } from "../protocol/types";
import { AgentViewActions, pushEvent } from "./agentView";
import { chatPanelHtml } from "./chatHtml";

/**
 * Floating chat tab — carried over from the legacy prism-agent-chat
 * "openFloatingChat" panel, but talking to this extension's own backend
 * handle instead of spawning a second `prism backend` child process.
 */
export class ChatTabPanel {
  private static current: ChatTabPanel | undefined;

  static createOrShow(
    extensionUri: vscode.Uri,
    actions: AgentViewActions,
    snapshot: BackendNotification[],
    backendState: string
  ): void {
    if (ChatTabPanel.current) {
      ChatTabPanel.current.panel.reveal(vscode.ViewColumn.Beside);
      return;
    }
    const panel = vscode.window.createWebviewPanel(
      "prism.chatTab",
      "PRISM Agent",
      vscode.ViewColumn.Beside,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [extensionUri],
      }
    );
    ChatTabPanel.current = new ChatTabPanel(panel, actions, snapshot, backendState);
  }

  private readonly events: BackendNotification[] = [];

  private constructor(
    private readonly panel: vscode.WebviewPanel,
    private readonly actions: AgentViewActions,
    snapshot: BackendNotification[],
    backendState: string
  ) {
    for (const notification of snapshot) {
      pushEvent(this.events, notification);
    }
    panel.webview.html = chatPanelHtml(
      panel.webview,
      JSON.stringify(this.events)
    );
    this.postStatus(`Backend ${backendState}`);
    panel.webview.onDidReceiveMessage((message: unknown) => {
      void this.handleMessage(message);
    });
    panel.onDidDispose(() => {
      ChatTabPanel.current = undefined;
    });
  }

  postNotification(notification: BackendNotification): void {
    pushEvent(this.events, notification);
    void this.panel.webview.postMessage({ type: "notification", notification });
  }

  postStatus(status: string): void {
    void this.panel.webview.postMessage({ type: "status", status });
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
}
