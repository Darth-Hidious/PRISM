import * as vscode from "vscode";
import { readAccountInfo } from "./account";
import { PrismBackend } from "./backend/prismBackend";
import { Marc27ApiClient } from "./marc27/apiClient";
import { Marc27Capabilities } from "./marc27/types";
import { BackendNotification } from "./protocol/types";
import {
  AgentViewActions,
  AgentViewProvider,
  CHAT_EVENT_LIMIT,
} from "./views/agentView";
import { ChatTabPanel } from "./views/chatTab";
import {
  buildAskQuery,
  MaterialsViewProvider,
} from "./views/materialsView";
import { PrismTreeEntry, PrismTreeProvider } from "./views/prismTree";
import { ToolsTreeProvider } from "./views/toolsTree";
import { WelcomePanel } from "./views/welcomePanel";

let capabilities: Marc27Capabilities | undefined;
let warnedLegacyApiSetting = false;

function configuredPlatformApiBaseUrl(
  output: vscode.OutputChannel
): string {
  const config = vscode.workspace.getConfiguration("prism");
  const native = config.get<string>("apiBaseUrl")?.trim();
  if (native) {
    return native;
  }
  const legacy = config.get<string>("marc27ApiBaseUrl")?.trim();
  if (legacy && !warnedLegacyApiSetting) {
    warnedLegacyApiSetting = true;
    output.appendLine(
      "warning: prism.marc27ApiBaseUrl is deprecated; use prism.apiBaseUrl instead."
    );
  }
  return legacy ?? "";
}

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("PRISM");
  const backend = new PrismBackend(output);
  const api = new Marc27ApiClient(context.secrets, () =>
    configuredPlatformApiBaseUrl(output)
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

  const actions: AgentViewActions = {
    startBackend: () => startBackend(backend),
    sendMessage: (text) => backend.sendMessage(text).then(() => undefined),
    sendCommand: (command) => backend.sendCommand(command).then(() => undefined),
    approve: (response) =>
      backend.respondToApproval(response).then(() => undefined),
  };
  const agentView = new AgentViewProvider(context.extensionUri, actions);

  // Rolling buffer of backend events, shared with late-opened chat tabs.
  const eventLog: BackendNotification[] = [];

  const materialsView = new MaterialsViewProvider(context.extensionUri, {
    search: (query) => backend.sendMessage(query).then(() => undefined),
    ask: (composition) =>
      backend.sendMessage(buildAskQuery(composition)).then(() => undefined),
  });

  const toolsTree = new ToolsTreeProvider();

  backend.onStateChanged((state) => {
    status.text =
      state === "running" ? "$(beaker) PRISM: Ready" : "$(beaker) PRISM";
    agentView.postStatus(`Backend ${state}`);
    refreshTrees();
  });
  backend.onNotification((notification) => {
    eventLog.push(notification);
    if (eventLog.length > CHAT_EVENT_LIMIT) {
      eventLog.shift();
    }
    agentView.append(notification);
    if (notification.method === "ui.cost") {
      status.tooltip = `Last cost event: ${JSON.stringify(notification.params)}`;
    } else if (notification.method === "ui.tools.catalog") {
      // Structured tool catalog emitted by `/tools`
      // (crates/agent/src/protocol.rs) — drives the Tools tree.
      toolsTree.setCatalog(notification.params);
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
    vscode.window.registerWebviewViewProvider(
      MaterialsViewProvider.viewType,
      materialsView
    ),
    vscode.window.registerTreeDataProvider("prism.context", contextTree),
    vscode.window.registerTreeDataProvider("prism.models", modelsTree),
    vscode.window.registerTreeDataProvider("prism.workflows", workflowsTree),
    vscode.window.registerTreeDataProvider("prism.jobs", jobsTree),
    vscode.window.registerTreeDataProvider("prism.billing", billingTree),
    vscode.window.registerTreeDataProvider("prism.tools", toolsTree),
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
        `Platform capabilities loaded: ${capabilities.total_endpoints} endpoints.`
      );
    }),
    vscode.commands.registerCommand("prism.setMarc27ApiKey", async () => {
      const value = await vscode.window.showInputBox({
        title: "Set Platform API Key",
        password: true,
        ignoreFocusOut: true,
        prompt: "Stored in VS Code SecretStorage, never in workspace files.",
      });
      if (value) {
        await api.setApiKey(value);
        refreshTrees();
        vscode.window.showInformationMessage("Platform API key stored.");
      }
    }),
    vscode.commands.registerCommand("prism.clearMarc27ApiKey", async () => {
      await api.clearApiKey();
      refreshTrees();
      vscode.window.showInformationMessage("Platform API key cleared.");
    }),

    // ── Folded in from the legacy marc27.* suite (verified against the
    //    current backend protocol before wiring) ────────────────────────

    // prism-agent-chat: new chat = /clear (native slash command).
    vscode.commands.registerCommand("prism.newChat", async () => {
      await backend.sendCommand("/clear");
      await vscode.commands.executeCommand("prism.openAgent");
    }),
    // prism-agent-chat: floating chat tab sharing this backend.
    vscode.commands.registerCommand("prism.openChatTab", () => {
      ChatTabPanel.createOrShow(
        context.extensionUri,
        actions,
        eventLog,
        backend.currentState
      );
    }),
    // prism-agent-chat: auto-approve toggle. The backend reads auto_approve
    // only at `init`, so this takes effect on the next backend start.
    vscode.commands.registerCommand("prism.toggleAutoApprove", async () => {
      const config = vscode.workspace.getConfiguration("prism");
      const next = !config.get<boolean>("autoApprove", false);
      await config.update(
        "autoApprove",
        next,
        vscode.ConfigurationTarget.Global
      );
      void vscode.window.showInformationMessage(
        `PRISM auto-approve: ${next ? "ON" : "OFF"}` +
          (backend.currentState === "running"
            ? " — applies when the backend next starts."
            : "")
      );
    }),
    // prism-agent-chat: ask about the whole active file.
    vscode.commands.registerCommand("prism.askAboutFile", () =>
      askAboutFile(backend)
    ),
    // prism-agent-chat: model override — /model is a native slash command.
    vscode.commands.registerCommand("prism.setModel", () =>
      runPromptedCommand(backend, "Model id (empty shows current)", "/model")
    ),

    // prism-marketplace: real capability today is the CLI-backed
    // /marketplace slash root (search/install/find), not the removed
    // unauthenticated HTTP endpoints.
    vscode.commands.registerCommand("prism.marketplaceSearch", () =>
      runPromptedCommand(backend, "Marketplace search", "/marketplace search")
    ),
    vscode.commands.registerCommand("prism.marketplaceInstall", () =>
      runPromptedCommand(
        backend,
        "Marketplace install (slug)",
        "/marketplace install"
      )
    ),
    vscode.commands.registerCommand("prism.marketplaceFind", () =>
      runPromptedCommand(
        backend,
        "Semantic marketplace discovery",
        "/marketplace find"
      )
    ),
    vscode.commands.registerCommand("prism.refreshTools", async () => {
      await backend.sendCommand("/tools");
      await vscode.commands.executeCommand("prism.tools.focus");
    }),

    // prism-mesh: CLI-backed /mesh root plus the platform node registry.
    vscode.commands.registerCommand("prism.meshHealth", () =>
      backend.sendCommand("/mesh health")
    ),
    vscode.commands.registerCommand("prism.meshPeers", () =>
      backend.sendCommand("/mesh peers")
    ),
    vscode.commands.registerCommand("prism.meshDiscover", () =>
      backend.sendCommand("/mesh discover")
    ),
    vscode.commands.registerCommand("prism.listNodes", async () => {
      await backend.sendCommand("/nodes");
      await vscode.commands.executeCommand("prism.openAgent");
    }),

    // prism-auth: read-only account status from the CLI's state file, and
    // terminal-driven sign in/out (the user runs the CLI themselves).
    vscode.commands.registerCommand("prism.accountStatus", () => {
      const account = readAccountInfo();
      if (!account) {
        void vscode.window.showInformationMessage(
          "Not signed in. Run `prism login` in a terminal to authenticate."
        );
        return;
      }
      const org = account.orgName ? ` (${account.orgName})` : "";
      const project = account.projectName ? ` — project ${account.projectName}` : "";
      void vscode.window.showInformationMessage(
        `Signed in as ${account.displayName}${org}${project}`
      );
    }),
    vscode.commands.registerCommand("prism.signIn", async () => {
      const action = await vscode.window.showInformationMessage(
        "Sign in through the PRISM CLI.",
        {
          detail:
            "This opens a terminal and runs `prism login` — follow its prompts.",
        },
        "Open Terminal"
      );
      if (action === "Open Terminal") {
        const terminal = vscode.window.createTerminal("PRISM Login");
        terminal.show();
        terminal.sendText("prism login");
      }
    }),
    vscode.commands.registerCommand("prism.signOut", async () => {
      const action = await vscode.window.showInformationMessage(
        "Sign out of the platform?",
        { detail: "This opens a terminal and runs `prism logout`." },
        "Open Terminal"
      );
      if (action === "Open Terminal") {
        const terminal = vscode.window.createTerminal("PRISM Logout");
        terminal.show();
        terminal.sendText("prism logout");
      }
    }),

    // prism-materials: periodic table and composition queries.
    vscode.commands.registerCommand("prism.openPeriodicTable", async () => {
      await vscode.commands.executeCommand("workbench.view.extension.prism");
      await vscode.commands.executeCommand("prism.materials.focus");
    }),
    vscode.commands.registerCommand("prism.searchMaterials", () =>
      materialsView.promptSearch()
    ),
    vscode.commands.registerCommand("prism.askAboutComposition", async () => {
      const composition = materialsView.currentComposition;
      if (!composition) {
        void vscode.window.showWarningMessage(
          "Select elements in the periodic table first."
        );
        return;
      }
      await backend.sendMessage(buildAskQuery(composition));
      await vscode.commands.executeCommand("prism.openAgent");
    }),

    // prism-welcome: offline welcome tab.
    vscode.commands.registerCommand("prism.showWelcome", () =>
      WelcomePanel.createOrShow(context.extensionUri, {
        sendMessage: (text) => backend.sendMessage(text).then(() => undefined),
      })
    )
  );

  const welcomeConfig = vscode.workspace.getConfiguration("prism.welcome");
  if (welcomeConfig.get<boolean>("showOnStartup", false)) {
    globalThis.setTimeout(() => {
      WelcomePanel.createOrShow(context.extensionUri, {
        sendMessage: (text) => backend.sendMessage(text).then(() => undefined),
      });
    }, 500);
  }
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

async function askAboutFile(backend: PrismBackend): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor) {
    await vscode.window.showWarningMessage("Open a file first.");
    return;
  }
  const file = editor.document.uri.fsPath;
  const language = editor.document.languageId;
  await backend.sendMessage(
    `What does the file ${file} do? Here is its content:\n\n` +
      "```" +
      language +
      "\n" +
      editor.document.getText() +
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
  if (text === undefined) {
    return;
  }
  const argument = text.trim();
  await backend.sendCommand(argument ? `${slashCommand} ${argument}` : slashCommand);
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
    `Query the platform knowledge graph and semantic corpus for: ${text.trim()}`
  );
  await vscode.commands.executeCommand("prism.openAgent");
}

async function contextEntries(
  backend: PrismBackend,
  api: Marc27ApiClient
): Promise<PrismTreeEntry[]> {
  const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? "No folder";
  const hasApiKey = await api.hasApiKey();
  const account = readAccountInfo();
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
      label: "Account",
      description: account ? account.displayName : "not signed in",
      icon: new vscode.ThemeIcon(account ? "account" : "sign-in"),
      children: [
        {
          label: account ? "Account status" : "Sign in (prism login)",
          icon: new vscode.ThemeIcon(account ? "info" : "sign-in"),
          command: {
            title: "Account",
            command: account ? "prism.accountStatus" : "prism.signIn",
          },
        },
        ...(account
          ? [
              {
                label: "Sign out",
                icon: new vscode.ThemeIcon("sign-out"),
                command: { title: "Sign out", command: "prism.signOut" },
              },
            ]
          : []),
      ],
    },
    {
      label: "Platform API",
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
          title: "Refresh platform capabilities",
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
