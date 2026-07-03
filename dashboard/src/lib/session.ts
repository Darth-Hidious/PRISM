const STORAGE_KEY = "prism.dashboard.session";

function sanitizeToken(value: string | null | undefined): string | null {
  const token = value?.trim();
  return token ? token : null;
}

export function getSessionToken(): string | null {
  if (typeof window === "undefined") {
    return null;
  }
  return sanitizeToken(window.sessionStorage.getItem(STORAGE_KEY));
}

export function setSessionToken(token: string): void {
  if (typeof window === "undefined") {
    return;
  }
  const sanitized = sanitizeToken(token);
  if (sanitized) {
    window.sessionStorage.setItem(STORAGE_KEY, sanitized);
  }
}

export function clearSessionToken(): void {
  if (typeof window === "undefined") {
    return;
  }
  window.sessionStorage.removeItem(STORAGE_KEY);
}

export function bootstrapSessionTokenFromUrl(): string | null {
  if (typeof window === "undefined") {
    return null;
  }

  const url = new URL(window.location.href);
  const token =
    sanitizeToken(url.searchParams.get("token")) ??
    sanitizeToken(url.searchParams.get("session")) ??
    sanitizeToken(url.searchParams.get("session_id"));

  if (!token) {
    return getSessionToken();
  }

  setSessionToken(token);
  url.searchParams.delete("token");
  url.searchParams.delete("session");
  url.searchParams.delete("session_id");
  window.history.replaceState({}, "", url.toString());
  return token;
}
