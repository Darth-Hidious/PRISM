export interface Marc27Endpoint {
  method: string;
  path: string;
  description: string;
  example_body?: string;
}

export interface Marc27Service {
  endpoint_count: number;
  endpoints: Marc27Endpoint[];
}

export interface Marc27Capabilities {
  platform: string;
  version: string;
  description: string;
  total_endpoints: number;
  auth: Record<string, unknown>;
  services: Record<string, Marc27Service>;
  graphql?: Record<string, unknown>;
  error_handling?: Record<string, unknown>;
  cli?: Record<string, unknown>;
}

export interface Marc27ApiError {
  error?: {
    code?: string;
    message?: string;
  };
  help?: Record<string, unknown>;
  suggestions?: unknown[];
}

export interface BillingPackage {
  slug?: string;
  name?: string;
  credits?: number;
  price_usd?: number;
  [key: string]: unknown;
}
