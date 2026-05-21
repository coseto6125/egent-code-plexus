export interface User {
  id: number;
  email: string;
  role: string;
}

export function authenticate(token: string): User | null {
  if (!token) return null;
  return { id: 1, email: "user@example.com", role: "admin" };
}

export async function refreshToken(userId: number): Promise<string> {
  return `token-${userId}`;
}
