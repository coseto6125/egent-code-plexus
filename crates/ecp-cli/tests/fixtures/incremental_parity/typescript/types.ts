export type Status = "active" | "inactive" | "pending";

export interface Config {
  timeout: number;
  retries: number;
  baseUrl: string;
}

export type Handler<T> = (input: T) => Promise<void>;

export function defaultConfig(): Config {
  return { timeout: 5000, retries: 3, baseUrl: "http://localhost" };
}
