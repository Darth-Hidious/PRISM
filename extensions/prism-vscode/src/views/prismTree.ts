import * as vscode from "vscode";

export interface PrismTreeEntry {
  label: string;
  description?: string;
  detail?: string;
  icon?: vscode.ThemeIcon;
  command?: vscode.Command;
  children?: PrismTreeEntry[];
}

export class PrismTreeItem extends vscode.TreeItem {
  constructor(private readonly entry: PrismTreeEntry) {
    super(
      entry.label,
      entry.children?.length
        ? vscode.TreeItemCollapsibleState.Collapsed
        : vscode.TreeItemCollapsibleState.None
    );
    this.description = entry.description;
    this.tooltip = entry.detail ?? entry.description;
    this.iconPath = entry.icon;
    this.command = entry.command;
  }

  get children(): PrismTreeEntry[] {
    return this.entry.children ?? [];
  }
}

export class PrismTreeProvider
  implements vscode.TreeDataProvider<PrismTreeItem>
{
  private readonly changeEmitter =
    new vscode.EventEmitter<PrismTreeItem | undefined | null | void>();

  readonly onDidChangeTreeData = this.changeEmitter.event;

  constructor(
    private readonly getEntries: () => PrismTreeEntry[] | Promise<PrismTreeEntry[]>
  ) {}

  refresh(): void {
    this.changeEmitter.fire();
  }

  getTreeItem(element: PrismTreeItem): vscode.TreeItem {
    return element;
  }

  async getChildren(element?: PrismTreeItem): Promise<PrismTreeItem[]> {
    const entries = element ? element.children : await this.getEntries();
    return entries.map((entry) => new PrismTreeItem(entry));
  }
}
