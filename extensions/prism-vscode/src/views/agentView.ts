import * as vscode from "vscode";
import { BackendNotification } from "../protocol/types";
import { chatPanelHtml } from "./chatHtml";

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
    webviewView.webview.html = chatPanelHtml(
      webviewView.webview,
      JSON.stringify(this.events)
    );
    webviewView.webview.onDidReceiveMessage((message: unknown) => {
      void this.handleMessage(message);
    });
  }

  append(notification: BackendNotification): void {
    pushEvent(this.events, notification);
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
}

export const CHAT_EVENT_LIMIT = 120;

export function pushEvent(
  events: BackendNotification[],
  notification: BackendNotification
): void {
  events.push(notification);
  if (events.length > CHAT_EVENT_LIMIT) {
    events.shift();
  }
}
