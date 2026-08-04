import * as vscode from "vscode";
import { nonceValue } from "./chatHtml";

export interface MaterialsActions {
  search: (query: string) => Promise<void>;
  ask: (composition: string) => Promise<void>;
}

/**
 * Interactive periodic table and composition builder — carried over from the
 * legacy prism-materials extension. The table itself is pure client-side;
 * search/ask now go through this extension's own backend connection
 * (`input.message`) instead of another extension's commands.
 */
export class MaterialsViewProvider implements vscode.WebviewViewProvider {
  static readonly viewType = "prism.materials";

  private view: vscode.WebviewView | undefined;
  private composition = "";

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly actions: MaterialsActions
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

  get currentComposition(): string {
    return this.composition;
  }

  /** Command palette entry: ask the user for a composition, then search. */
  async promptSearch(): Promise<void> {
    const value = await vscode.window.showInputBox({
      title: "Search Materials",
      prompt: "Composition or material, e.g. Fe2O3, NiTi, perovskite",
      ignoreFocusOut: true,
    });
    if (!value?.trim()) {
      return;
    }
    this.composition = value.trim();
    await this.actions.search(buildSearchQuery(value.trim(), undefined));
  }

  private async handleMessage(message: unknown): Promise<void> {
    if (typeof message !== "object" || message === null) {
      return;
    }
    const payload = message as {
      type?: string;
      composition?: string;
      filters?: Record<string, string>;
    };
    if (payload.type === "composition-changed") {
      this.composition = payload.composition ?? "";
    } else if (payload.type === "search" && payload.composition?.trim()) {
      this.composition = payload.composition;
      try {
        await this.actions.search(
          buildSearchQuery(payload.composition, payload.filters)
        );
        await vscode.commands.executeCommand("prism.openAgent");
      } catch (error) {
        await vscode.window.showErrorMessage(String(error));
      }
    } else if (payload.type === "ask" && payload.composition?.trim()) {
      this.composition = payload.composition;
      try {
        await this.actions.ask(payload.composition);
        await vscode.commands.executeCommand("prism.openAgent");
      } catch (error) {
        await vscode.window.showErrorMessage(String(error));
      }
    }
  }

  private html(webview: vscode.Webview): string {
    const nonce = nonceValue();
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'nonce-${nonce}'; script-src 'nonce-${nonce}';">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <style nonce="${nonce}">
    :root { color-scheme: light dark; }
    body {
      margin: 0; padding: 10px;
      color: var(--vscode-foreground);
      background: var(--vscode-sideBar-background);
      font: var(--vscode-font-size) var(--vscode-font-family);
    }
    h3 { margin: 10px 0 6px; font-size: 12px; text-transform: uppercase; opacity: 0.75; }
    #table { display: grid; grid-template-columns: repeat(18, minmax(20px, 1fr)); gap: 2px; }
    .el {
      border: 0; border-radius: 3px; padding: 3px 0; cursor: pointer;
      font-size: 10px; line-height: 1.1; text-align: center; color: #101014;
    }
    .el:hover { outline: 1px solid var(--vscode-focusBorder); }
    .el .z { display: block; font-size: 7px; opacity: 0.7; }
    .cat0 { background: #f2a65a; } .cat1 { background: #f2cf5b; }
    .cat2 { background: #9ec1e0; } .cat3 { background: #a8d8a8; }
    .cat4 { background: #c9b3d6; } .cat5 { background: #f5f0a1; }
    .cat6 { background: #f28b82; } .cat7 { background: #b39ddb; }
    .cat8 { background: #f8a7c2; } .cat9 { background: #e5989b; }
    #spacer { height: 8px; }
    #composition { min-height: 26px; padding: 6px; border: 1px solid var(--vscode-panel-border); border-radius: 4px; }
    #composition .empty { opacity: 0.55; font-size: 12px; }
    .chip {
      display: inline-flex; align-items: center; gap: 4px; margin: 2px;
      padding: 2px 6px; border-radius: 10px; background: var(--vscode-badge-background);
      color: var(--vscode-badge-foreground); font-size: 12px;
    }
    .chip button { border: 0; background: none; color: inherit; cursor: pointer; padding: 0 2px; }
    .filter-row { display: flex; gap: 6px; align-items: center; margin: 4px 0; font-size: 12px; }
    .filter-row label { flex: 1; opacity: 0.8; }
    .filter-row input {
      width: 60px; color: var(--vscode-input-foreground);
      background: var(--vscode-input-background);
      border: 1px solid var(--vscode-input-border, transparent); border-radius: 3px;
    }
    .btnrow { display: flex; gap: 8px; margin-top: 8px; }
    .btnrow button {
      flex: 1; padding: 6px; cursor: pointer; border-radius: 4px;
      border: 1px solid var(--vscode-button-border, transparent);
      color: var(--vscode-button-foreground); background: var(--vscode-button-background);
    }
    .btnrow button.secondary { background: var(--vscode-button-secondaryBackground, #444); }
    #hint { font-size: 11px; opacity: 0.6; margin-top: 6px; }
  </style>
</head>
<body>
  <div id="table"></div>
  <div id="spacer"></div>
  <h3>Composition</h3>
  <div id="composition"><span class="empty">Click elements above to build a composition</span></div>
  <h3>Property Filters</h3>
  <div class="filter-row"><label>Melting point (K)</label>
    <input id="f-mp-min" type="number" placeholder="min"><span>&ndash;</span><input id="f-mp-max" type="number" placeholder="max"></div>
  <div class="filter-row"><label>Density (g/cm3)</label>
    <input id="f-d-min" type="number" placeholder="min"><span>&ndash;</span><input id="f-d-max" type="number" placeholder="max"></div>
  <div class="filter-row"><label>Band gap (eV)</label>
    <input id="f-bg-min" type="number" placeholder="min"><span>&ndash;</span><input id="f-bg-max" type="number" placeholder="max"></div>
  <div class="btnrow">
    <button id="search">Search Materials</button>
    <button id="ask" class="secondary">Ask Agent</button>
    <button id="clear" class="secondary" title="Clear composition">&#10005;</button>
  </div>
  <div id="hint">Search sends a structured query to the PRISM agent; results stream into the Agent view.</div>
  <script nonce="${nonce}">
    const vscodeApi = acquireVsCodeApi();
    const ELEMENTS = ${JSON.stringify(ELEMENT_DATA)};

    function positionOf(z) {
      if (z === 1) return [1, 1];
      if (z === 2) return [1, 18];
      if (z <= 10) return [2, z <= 4 ? z - 2 : z + 8];
      if (z <= 18) return [3, z <= 12 ? z - 10 : z];
      if (z <= 36) return [4, z - 18];
      if (z <= 54) return [5, z - 36];
      if (z <= 56) return [6, z - 54];
      if (z <= 71) return [8, z - 54];
      if (z <= 86) return [6, z - 68];
      if (z <= 88) return [7, z - 86];
      if (z <= 103) return [9, z - 86];
      return [7, z - 100];
    }

    const tableEl = document.getElementById("table");
    const compEl = document.getElementById("composition");
    const counts = new Map();

    ELEMENTS.forEach((entry, index) => {
      const z = index + 1;
      const [symbol, name, cat] = entry;
      const [row, col] = positionOf(z);
      const cell = document.createElement("button");
      cell.type = "button";
      cell.className = "el cat" + cat;
      cell.style.gridRow = row;
      cell.style.gridColumn = col;
      cell.title = name + " (" + z + ")";
      const zSpan = document.createElement("span");
      zSpan.className = "z";
      zSpan.textContent = String(z);
      cell.appendChild(zSpan);
      cell.appendChild(document.createTextNode(symbol));
      cell.addEventListener("click", () => {
        counts.set(symbol, (counts.get(symbol) || 0) + 1);
        renderComposition();
      });
      tableEl.appendChild(cell);
    });

    function compositionString() {
      let out = "";
      for (const [symbol, count] of counts) {
        out += symbol + (count > 1 ? count : "");
      }
      return out;
    }

    function renderComposition() {
      compEl.innerHTML = "";
      if (counts.size === 0) {
        const empty = document.createElement("span");
        empty.className = "empty";
        empty.textContent = "Click elements above to build a composition";
        compEl.appendChild(empty);
      } else {
        for (const [symbol, count] of counts) {
          const chip = document.createElement("span");
          chip.className = "chip";
          chip.textContent = symbol + (count > 1 ? count : "");
          const remove = document.createElement("button");
          remove.textContent = "\\u00d7";
          remove.title = "Remove " + symbol;
          remove.addEventListener("click", () => {
            counts.delete(symbol);
            renderComposition();
          });
          chip.appendChild(remove);
          compEl.appendChild(chip);
        }
      }
      vscodeApi.postMessage({ type: "composition-changed", composition: compositionString() });
    }

    function currentFilters() {
      const filters = {};
      const pairs = [
        ["melting_point_k_min", "f-mp-min"], ["melting_point_k_max", "f-mp-max"],
        ["density_g_cm3_min", "f-d-min"], ["density_g_cm3_max", "f-d-max"],
        ["band_gap_ev_min", "f-bg-min"], ["band_gap_ev_max", "f-bg-max"],
      ];
      for (const [key, id] of pairs) {
        const value = document.getElementById(id).value.trim();
        if (value) filters[key] = value;
      }
      return filters;
    }

    document.getElementById("search").addEventListener("click", () => {
      const composition = compositionString();
      if (!composition) return;
      vscodeApi.postMessage({ type: "search", composition, filters: currentFilters() });
    });
    document.getElementById("ask").addEventListener("click", () => {
      const composition = compositionString();
      if (!composition) return;
      vscodeApi.postMessage({ type: "ask", composition });
    });
    document.getElementById("clear").addEventListener("click", () => {
      counts.clear();
      renderComposition();
    });
    renderComposition();
  </script>
</body>
</html>`;
  }
}

export function buildSearchQuery(
  composition: string,
  filters: Record<string, string> | undefined
): string {
  let query = `Search for materials with composition: ${composition}`;
  if (filters) {
    const parts = Object.entries(filters)
      .filter(([, value]) => value !== undefined && value !== "")
      .map(([key, value]) => `${key}: ${value}`);
    if (parts.length > 0) {
      query += `\nProperty filters: ${parts.join(", ")}`;
    }
  }
  query +=
    "\nReturn a structured list of matching materials with their key properties.";
  return query;
}

export function buildAskQuery(composition: string): string {
  return (
    "Analyze this material composition and suggest properties, applications, " +
    `and similar alloys:\n${composition}`
  );
}

/**
 * [symbol, name, category] for Z = 1..118. Category palette:
 * 0 alkali, 1 alkaline earth, 2 transition, 3 post-transition, 4 metalloid,
 * 5 reactive nonmetal, 6 halogen, 7 noble gas, 8 lanthanide, 9 actinide.
 */
const ELEMENT_DATA: Array<[string, string, number]> = [
  ["H", "Hydrogen", 5], ["He", "Helium", 7],
  ["Li", "Lithium", 0], ["Be", "Beryllium", 1], ["B", "Boron", 4],
  ["C", "Carbon", 5], ["N", "Nitrogen", 5], ["O", "Oxygen", 5],
  ["F", "Fluorine", 6], ["Ne", "Neon", 7],
  ["Na", "Sodium", 0], ["Mg", "Magnesium", 1], ["Al", "Aluminium", 3],
  ["Si", "Silicon", 4], ["P", "Phosphorus", 5], ["S", "Sulfur", 5],
  ["Cl", "Chlorine", 6], ["Ar", "Argon", 7],
  ["K", "Potassium", 0], ["Ca", "Calcium", 1],
  ["Sc", "Scandium", 2], ["Ti", "Titanium", 2], ["V", "Vanadium", 2],
  ["Cr", "Chromium", 2], ["Mn", "Manganese", 2], ["Fe", "Iron", 2],
  ["Co", "Cobalt", 2], ["Ni", "Nickel", 2], ["Cu", "Copper", 2],
  ["Zn", "Zinc", 2],
  ["Ga", "Gallium", 3], ["Ge", "Germanium", 4], ["As", "Arsenic", 4],
  ["Se", "Selenium", 5], ["Br", "Bromine", 6], ["Kr", "Krypton", 7],
  ["Rb", "Rubidium", 0], ["Sr", "Strontium", 1],
  ["Y", "Yttrium", 2], ["Zr", "Zirconium", 2], ["Nb", "Niobium", 2],
  ["Mo", "Molybdenum", 2], ["Tc", "Technetium", 2], ["Ru", "Ruthenium", 2],
  ["Rh", "Rhodium", 2], ["Pd", "Palladium", 2], ["Ag", "Silver", 2],
  ["Cd", "Cadmium", 2],
  ["In", "Indium", 3], ["Sn", "Tin", 3], ["Sb", "Antimony", 4],
  ["Te", "Tellurium", 4], ["I", "Iodine", 6], ["Xe", "Xenon", 7],
  ["Cs", "Caesium", 0], ["Ba", "Barium", 1],
  ["La", "Lanthanum", 8], ["Ce", "Cerium", 8], ["Pr", "Praseodymium", 8],
  ["Nd", "Neodymium", 8], ["Pm", "Promethium", 8], ["Sm", "Samarium", 8],
  ["Eu", "Europium", 8], ["Gd", "Gadolinium", 8], ["Tb", "Terbium", 8],
  ["Dy", "Dysprosium", 8], ["Ho", "Holmium", 8], ["Er", "Erbium", 8],
  ["Tm", "Thulium", 8], ["Yb", "Ytterbium", 8], ["Lu", "Lutetium", 8],
  ["Hf", "Hafnium", 2], ["Ta", "Tantalum", 2], ["W", "Tungsten", 2],
  ["Re", "Rhenium", 2], ["Os", "Osmium", 2], ["Ir", "Iridium", 2],
  ["Pt", "Platinum", 2], ["Au", "Gold", 2], ["Hg", "Mercury", 2],
  ["Tl", "Thallium", 3], ["Pb", "Lead", 3], ["Bi", "Bismuth", 3],
  ["Po", "Polonium", 3], ["At", "Astatine", 6], ["Rn", "Radon", 7],
  ["Fr", "Francium", 0], ["Ra", "Radium", 1],
  ["Ac", "Actinium", 9], ["Th", "Thorium", 9], ["Pa", "Protactinium", 9],
  ["U", "Uranium", 9], ["Np", "Neptunium", 9], ["Pu", "Plutonium", 9],
  ["Am", "Americium", 9], ["Cm", "Curium", 9], ["Bk", "Berkelium", 9],
  ["Cf", "Californium", 9], ["Es", "Einsteinium", 9], ["Fm", "Fermium", 9],
  ["Md", "Mendelevium", 9], ["No", "Nobelium", 9], ["Lr", "Lawrencium", 9],
  ["Rf", "Rutherfordium", 2], ["Db", "Dubnium", 2], ["Sg", "Seaborgium", 2],
  ["Bh", "Bohrium", 2], ["Hs", "Hassium", 2], ["Mt", "Meitnerium", 2],
  ["Ds", "Darmstadtium", 2], ["Rg", "Roentgenium", 2], ["Cn", "Copernicium", 2],
  ["Nh", "Nihonium", 3], ["Fl", "Flerovium", 3], ["Mc", "Moscovium", 3],
  ["Lv", "Livermorium", 3], ["Ts", "Tennessine", 6], ["Og", "Oganesson", 7]
];
