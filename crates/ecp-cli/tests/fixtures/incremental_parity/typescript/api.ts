import { authenticate } from "./auth";

export class ApiClient {
  private baseUrl: string;

  constructor(baseUrl: string) {
    this.baseUrl = baseUrl;
  }

  async get(path: string): Promise<unknown> {
    const response = await fetch(`${this.baseUrl}${path}`);
    return response.json();
  }

  async post(path: string, data: unknown): Promise<unknown> {
    const response = await fetch(`${this.baseUrl}${path}`, {
      method: "POST",
      body: JSON.stringify(data),
    });
    return response.json();
  }
}

export function buildHeaders(token: string): Record<string, string> {
  const user = authenticate(token);
  return user ? { Authorization: `Bearer ${token}` } : {};
}
