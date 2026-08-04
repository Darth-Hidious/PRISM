import * as fs from "fs";
import * as os from "os";
import * as path from "path";

/**
 * Read-only account status sourced from the PRISM CLI's own state files.
 * Mirrors what the legacy prism-auth extension did, but against the paths
 * the current CLI actually writes:
 *
 * - cli-state.json under the OS config dir for bundle id dev.prism.prism
 *   (crates/runtime/src/lib.rs PrismPaths::cli_state_path)
 * - the ~/.prism/credentials.json SDK mirror as a fallback
 *   (PrismPaths::sdk_credentials_path)
 *
 * Tokens are never read into messages or logs — display fields only.
 */
export interface AccountInfo {
  displayName: string;
  orgName?: string;
  projectName?: string;
  platformUrl?: string;
  userId?: string;
  expiresAt?: string;
  source: string;
}

export function readAccountInfo(): AccountInfo | undefined {
  for (const candidate of cliStatePaths()) {
    const parsed = readJson(candidate);
    if (!parsed) {
      continue;
    }
    const creds = (parsed as { credentials?: Record<string, unknown> })
      .credentials;
    if (!creds || typeof creds.access_token !== "string") {
      continue;
    }
    return {
      displayName: stringField(creds, "display_name") ?? "Signed in",
      orgName: stringField(creds, "org_name"),
      projectName: stringField(creds, "project_name"),
      platformUrl: stringField(creds, "platform_url"),
      userId: stringField(creds, "user_id"),
      expiresAt: stringField(creds, "expires_at"),
      source: candidate,
    };
  }

  const mirror = readJson(sdkMirrorPath());
  if (mirror && typeof (mirror as { access_token?: unknown }).access_token === "string") {
    const record = mirror as Record<string, unknown>;
    return {
      displayName: stringField(record, "user_id") ?? "Signed in",
      platformUrl: stringField(record, "platform_url"),
      userId: stringField(record, "user_id"),
      expiresAt: stringField(record, "expires_at"),
      source: sdkMirrorPath(),
    };
  }
  return undefined;
}

function cliStatePaths(): string[] {
  const home = os.homedir();
  const paths: string[] = [];
  if (process.platform === "darwin") {
    paths.push(
      path.join(home, "Library", "Application Support", "dev.prism.prism", "cli-state.json")
    );
  } else if (process.platform === "win32" && process.env.APPDATA) {
    paths.push(path.join(process.env.APPDATA, "prism", "prism", "config", "cli-state.json"));
  } else {
    const configHome = process.env.XDG_CONFIG_HOME || path.join(home, ".config");
    paths.push(path.join(configHome, "prism", "cli-state.json"));
  }
  return paths;
}

function sdkMirrorPath(): string {
  return path.join(os.homedir(), ".prism", "credentials.json");
}

function readJson(file: string): unknown {
  try {
    return JSON.parse(fs.readFileSync(file, "utf-8")) as unknown;
  } catch {
    return undefined;
  }
}

function stringField(
  record: Record<string, unknown>,
  key: string
): string | undefined {
  const value = record[key];
  return typeof value === "string" && value.length > 0 ? value : undefined;
}
