import * as vscode from "vscode";

/**
 * One tool from the `ui.tools.catalog` notification the backend emits in
 * response to `/tools` (crates/agent/src/protocol.rs, `/tools` handler).
 */
export interface CatalogTool {
  name: string;
  description: string;
  approval: boolean;
  source: string;
  source_detail?: string;
}

export class ToolsTreeProvider implements vscode.TreeDataProvider<ToolItem> {
  private tools: CatalogTool[] = [];
  private readonly changeEmitter =
    new vscode.EventEmitter<ToolItem | undefined | null | void>();

  readonly onDidChangeTreeData = this.changeEmitter.event;

  /** Replace the catalog from a `ui.tools.catalog` notification. */
  setCatalog(params: unknown): void {
    const list =
      params && typeof params === "object"
        ? (params as { tools?: unknown }).tools
        : undefined;
    if (!Array.isArray(list)) {
      return;
    }
    this.tools = list
      .filter((entry): entry is Record<string, unknown> =>
        Boolean(entry && typeof entry === "object")
      )
      .map((entry) => ({
        name: String(entry.name ?? ""),
        description: String(entry.description ?? ""),
        approval: Boolean(entry.approval),
        source: String(entry.source ?? ""),
        source_detail:
          entry.source_detail === undefined
            ? undefined
            : String(entry.source_detail),
      }))
      .filter((tool) => tool.name.length > 0);
    this.changeEmitter.fire();
  }

  getTreeItem(element: ToolItem): vscode.TreeItem {
    return element;
  }

  getChildren(element?: ToolItem): ToolItem[] {
    if (element) {
      return [];
    }
    if (this.tools.length === 0) {
      const item = new vscode.TreeItem(
        "Run 'Refresh Agent Tools' to load the catalog"
      );
      item.iconPath = new vscode.ThemeIcon("info");
      return [item as unknown as ToolItem];
    }
    return this.tools.map((tool) => new ToolItem(tool));
  }
}

export class ToolItem extends vscode.TreeItem {
  constructor(tool: CatalogTool) {
    super(tool.name, vscode.TreeItemCollapsibleState.None);
    this.description = tool.approval ? "approval required" : "auto-approved";
    this.iconPath = new vscode.ThemeIcon(
      tool.approval ? "shield" : "tools",
      tool.approval
        ? new vscode.ThemeColor("charts.yellow")
        : new vscode.ThemeColor("charts.green")
    );
    this.tooltip = new vscode.MarkdownString(
      `${tool.description}\n\n- source: ${tool.source}` +
        (tool.source_detail ? ` (${tool.source_detail})` : "") +
        `\n- approval required: ${tool.approval}`
    );
  }
}
