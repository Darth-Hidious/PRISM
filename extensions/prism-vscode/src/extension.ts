import * as vscode from "vscode";
import { PrismBackend } from "./backend/prismBackend";
import { Marc27ApiClient } from "./marc27/apiClient";
import { Marc27Capabilities } from "./marc27/types";
import { AgentViewProvider } from "./views/agentView";
import { PrismTreeEntry, PrismTreeProvider } from "./views/prismTree";

let capabilities: Marc27Capabilities | undefined;

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("PRISM");
  const backend = new PrismBackend(output);
  const api = new Marc27ApiClient(context.secrets, () =>
    vscode.workspace
      .getConfiguration("prism")
      .get<string>("marc27ApiBaseUrl", "https://api.marc27.com/api/v1")
  );

  const status = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Left,
    80
  );
  status.name = "PRISM";
  status.command = "prism.openAgent";
  status.text = "$(beaker) PRISM";
  status.tooltip = "Open PRISM Agent";
  status.show();

  const agentView = new AgentViewProvider(context.extensionUri, {
    startBackend: () => startBackend(backend),
    sendMessage: (text) => backend.sendMessage(text).then(() => undefined),
    sendCommand: (command) => backend.sendCommand(command).then(() => undefined),
    approve: (response) =>
      backend.respondToApproval(response).then(() => undefined),
  });

  backend.onStateChanged((state) => {
    status.text =
      state === "running" ? "$(beaker) PRISM: Ready" : "$(beaker) PRISM";
    agentView.postStatus(`Backend ${state}`);
    refreshTrees();
  });
  backend.onNotification((notification) => {
    agentView.append(notification);
    if (notification.method === "ui.cost") {
      status.tooltip = `Last cost event: ${JSON.stringify(notification.params)}`;
    }
  });

  const contextTree = new PrismTreeProvider(() => contextEntries(backend, api));
  const modelsTree = new PrismTreeProvider(() => serviceEntries("llm", "Models"));
  const workflowsTree = new PrismTreeProvider(() =>
    serviceEntries("workflows", "Workflows")
  );
  const jobsTree = new PrismTreeProvider(() => serviceEntries("jobs", "Jobs"));
  const billingTree = new PrismTreeProvider(() =>
    serviceEntries("billing", "Billing")
  );
  const treeProviders = [
    contextTree,
    modelsTree,
    workflowsTree,
    jobsTree,
    billingTree,
  ];
  const refreshTrees = (): void => {
    for (const provider of treeProviders) {
      provider.refresh();
    }
  };

  context.subscriptions.push(
    output,
    backend,
    status,
    vscode.window.registerWebviewViewProvider("prism.agent", agentView),
    vscode.window.registerTreeDataProvider("prism.context", contextTree),
    vscode.window.registerTreeDataProvider("prism.models", modelsTree),
    vscode.window.registerTreeDataProvider("prism.workflows", workflowsTree),
    vscode.window.registerTreeDataProvider("prism.jobs", jobsTree),
    vscode.window.registerTreeDataProvider("prism.billing", billingTree),
    vscode.commands.registerCommand("prism.openAgent", async () => {
      await vscode.commands.executeCommand("workbench.view.extension.prism");
      await vscode.commands.executeCommand("prism.agent.focus");
    }),
    vscode.commands.registerCommand("prism.startBackend", () =>
      startBackend(backend)
    ),
    vscode.commands.registerCommand("prism.stopBackend", () => backend.stop()),
    vscode.commands.registerCommand("prism.sendSelection", () =>
      sendSelection(backend)
    ),
    vscode.commands.registerCommand("prism.runResearch", () =>
      runPromptedCommand(backend, "Research query", "/research")
    ),
    vscode.commands.registerCommand("prism.queryKnowledge", () =>
      queryKnowledge(backend)
    ),
    vscode.commands.registerCommand("prism.openModels", () =>
      backend.sendCommand("/models list")
    ),
    vscode.commands.registerCommand("prism.openWorkflows", () =>
      backend.sendCommand("/workflow list")
    ),
    vscode.commands.registerCommand("prism.refreshMarc27", async () => {
      capabilities = await api.capabilities();
      refreshTrees();
      vscode.window.showInformationMessage(
        `MARC27 capabilities loaded: ${capabilities.total_endpoints} endpoints.`
      );
    }),
    vscode.commands.registerCommand("prism.setMarc27ApiKey", async () => {
      const value = await vscode.window.showInputBox({
        title: "Set MARC27 API Key",
        password: true,
        ignoreFocusOut: true,
        prompt: "Stored in VS Code SecretStorage, never in workspace files.",
      });
      if (value) {
        await api.setApiKey(value);
        refreshTrees();
        vscode.window.showInformationMessage("MARC27 API key stored.");
      }
    }),
    vscode.commands.registerCommand("prism.clearMarc27ApiKey", async () => {
      await api.clearApiKey();
      refreshTrees();
      vscode.window.showInformationMessage("MARC27 API key cleared.");
    })
  );
}

export function deactivate(): void {
  // VS Code disposes extension subscriptions automatically.
}

async function startBackend(backend: PrismBackend): Promise<void> {
  await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: "Starting PRISM backend",
      cancellable: false,
    },
    async () => {
      await backend.start();
    }
  );
}

async function sendSelection(backend: PrismBackend): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor) {
    await vscode.window.showWarningMessage("Open a file and select text first.");
    return;
  }
  const selection = editor.selection;
  const text = editor.document.getText(selection);
  if (!text.trim()) {
    await vscode.window.showWarningMessage("Select text to send to PRISM.");
    return;
  }
  const language = editor.document.languageId;
  const file = editor.document.uri.fsPath;
  await backend.sendMessage(
    `Use this ${language} selection from ${file} as context:\n\n` +
      "```" +
      language +
      "\n" +
      text +
      "\n```"
  );
  await vscode.commands.executeCommand("prism.openAgent");
}

async function runPromptedCommand(
  backend: PrismBackend,
  title: string,
  slashCommand: string
): Promise<void> {
  const text = await vscode.window.showInputBox({
    title,
    ignoreFocusOut: true,
  });
  if (!text?.trim()) {
    return;
  }
  await backend.sendCommand(`${slashCommand} ${text.trim()}`);
  await vscode.commands.executeCommand("prism.openAgent");
}

async function queryKnowledge(backend: PrismBackend): Promise<void> {
  const text = await vscode.window.showInputBox({
    title: "Knowledge query",
    ignoreFocusOut: true,
  });
  if (!text?.trim()) {
    return;
  }
  await backend.sendMessage(
    `Query the MARC27 knowledge graph and semantic corpus for: ${text.trim()}`
  );
  await vscode.commands.executeCommand("prism.openAgent");
}

async function contextEntries(
  backend: PrismBackend,
  api: Marc27ApiClient
): Promise<PrismTreeEntry[]> {
  const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? "No folder";
  const hasApiKey = await api.hasApiKey();
  return [
    {
      label: "Backend",
      description: backend.currentState,
      icon: new vscode.ThemeIcon("server-process"),
      children: [
        {
          label: "Start backend",
          icon: new vscode.ThemeIcon("play"),
          command: {
            title: "Start backend",
            command: "prism.startBackend",
          },
        },
        {
          label: "Stop backend",
          icon: new vscode.ThemeIcon("debug-stop"),
          command: {
            title: "Stop backend",
            command: "prism.stopBackend",
          },
        },
      ],
    },
    {
      label: "Workspace",
      description: folder,
      icon: new vscode.ThemeIcon("root-folder"),
    },
    {
      label: "MARC27 API",
      description: hasApiKey ? "key stored" : "public discovery only",
      icon: new vscode.ThemeIcon(hasApiKey ? "lock" : "unlock"),
      children: [
        {
          label: "Refresh capabilities",
          icon: new vscode.ThemeIcon("refresh"),
          command: {
            title: "Refresh capabilities",
            command: "prism.refreshMarc27",
          },
        },
        {
          label: hasApiKey ? "Replace API key" : "Set API key",
          icon: new vscode.ThemeIcon("key"),
          command: {
            title: "Set API key",
            command: "prism.setMarc27ApiKey",
          },
        },
      ],
    },
  ];
}

function serviceEntries(serviceName: string, emptyLabel: string): PrismTreeEntry[] {
  const service = capabilities?.services?.[serviceName];
  if (!service) {
    return [
      {
        label: `Load ${emptyLabel}`,
        description: "refresh capabilities",
        icon: new vscode.ThemeIcon("cloud-download"),
        command: {
          title: "Refresh MARC27 capabilities",
          command: "prism.refreshMarc27",
        },
      },
    ];
  }

  return service.endpoints.map((endpoint) => ({
    label: `${endpoint.method} ${endpoint.path}`,
    description: endpoint.description,
    detail: endpoint.example_body,
    icon: new vscode.ThemeIcon(iconForMethod(endpoint.method)),
  }));
}

function iconForMethod(method: string): string {
  switch (method.toUpperCase()) {
    case "GET":
      return "arrow-down";
    case "POST":
      return "arrow-up";
    case "PUT":
    case "PATCH":
      return "edit";
    case "DELETE":
      return "trash";
    default:
      return "symbol-method";
  }
}
