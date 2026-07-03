import * as vscode from "vscode";
import { BillingPackage, Marc27ApiError, Marc27Capabilities } from "./types";

const API_KEY_SECRET = "marc27.apiKey";

export class Marc27ApiClient {
  constructor(
    private readonly secrets: vscode.SecretStorage,
    private readonly getBaseUrl: () => string
  ) {}

  async capabilities(): Promise<Marc27Capabilities> {
    return this.request<Marc27Capabilities>("/agent/capabilities", {
      auth: false,
    });
  }

  async billingPackages(): Promise<BillingPackage[]> {
    const body = await this.request<unknown>("/billing/packages", {
      auth: false,
    });
    if (Array.isArray(body)) {
      return body as BillingPackage[];
    }
    if (
      typeof body === "object" &&
      body !== null &&
      Array.isArray((body as { packages?: unknown }).packages)
    ) {
      return (body as { packages: BillingPackage[] }).packages;
    }
    return [];
  }

  async setApiKey(value: string): Promise<void> {
    await this.secrets.store(API_KEY_SECRET, value);
  }

  async clearApiKey(): Promise<void> {
    await this.secrets.delete(API_KEY_SECRET);
  }

  async hasApiKey(): Promise<boolean> {
    return Boolean(await this.secrets.get(API_KEY_SECRET));
  }

  private async request<T>(
    path: string,
    options: { method?: string; body?: unknown; auth?: boolean } = {}
  ): Promise<T> {
    const baseUrl = this.getBaseUrl().replace(/\/$/, "");
    const url = `${baseUrl}${path.startsWith("/") ? path : `/${path}`}`;
    const headers: Record<string, string> = {
      Accept: "application/json",
    };
    if (options.body !== undefined) {
      headers["Content-Type"] = "application/json";
    }
    if (options.auth !== false) {
      const token = await this.secrets.get(API_KEY_SECRET);
      if (token) {
        headers["X-API-Key"] = token;
      }
    }

    const response = await fetch(url, {
      method: options.method ?? "GET",
      headers,
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
    });
    const text = await response.text();
    const parsed = text ? (JSON.parse(text) as T | Marc27ApiError) : ({} as T);

    if (!response.ok) {
      const apiError = parsed as Marc27ApiError;
      const message =
        apiError.error?.message ??
        apiError.error?.code ??
        `${response.status} ${response.statusText}`;
      throw new Error(`MARC27 request failed: ${message}`);
    }

    return parsed as T;
  }
}
