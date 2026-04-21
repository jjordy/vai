const VAI_SERVER =
  typeof import.meta !== "undefined"
    ? (import.meta.env.VITE_VAI_SERVER_URL as string)
    : (process.env.VITE_VAI_SERVER_URL ?? "");

export interface ApiKey {
  id: string;
  name: string;
  key_prefix: string;
  agent_type: string | null;
  created_at: string;
}

export interface Repo {
  id: string;
  name: string;
}

export interface Issue {
  id: string;
  title: string;
}

export interface TokenResponse {
  access_token: string;
  token_type: string;
  expires_in: number;
  refresh_token?: string;
}

/** Exchange a Better Auth session token for a vai JWT. */
export async function exchangeSession(
  sessionToken: string
): Promise<string | null> {
  try {
    const res = await fetch(`${VAI_SERVER}/api/auth/token`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        grant_type: "session_exchange",
        session_token: sessionToken,
      }),
    });
    if (!res.ok) return null;
    const data: TokenResponse = await res.json();
    return data.access_token ?? null;
  } catch {
    return null;
  }
}

/** GET /api/keys — list the current user's API keys. */
export async function listKeys(vaiToken: string): Promise<ApiKey[]> {
  try {
    const res = await fetch(`${VAI_SERVER}/api/keys`, {
      headers: { Authorization: `Bearer ${vaiToken}` },
    });
    if (!res.ok) return [];
    return res.json();
  } catch {
    return [];
  }
}

/** GET /api/repos — list repos visible to the current user. */
export async function listRepos(vaiToken: string): Promise<Repo[]> {
  try {
    const res = await fetch(`${VAI_SERVER}/api/repos`, {
      headers: { Authorization: `Bearer ${vaiToken}` },
    });
    if (!res.ok) return [];
    return res.json();
  } catch {
    return [];
  }
}

/** GET /api/issues — list issues across all repos. */
export async function listIssues(vaiToken: string): Promise<Issue[]> {
  try {
    const res = await fetch(`${VAI_SERVER}/api/issues`, {
      headers: { Authorization: `Bearer ${vaiToken}` },
    });
    if (!res.ok) return [];
    return res.json();
  } catch {
    return [];
  }
}

/** POST /api/keys — mint a new API key. Returns the plaintext token + metadata. */
export async function createKey(
  vaiToken: string,
  body: { name: string; role?: string; agent_type?: string }
): Promise<{ key: ApiKey; token: string } | null> {
  try {
    const res = await fetch(`${VAI_SERVER}/api/keys`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${vaiToken}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(body),
    });
    if (!res.ok) return null;
    return res.json();
  } catch {
    return null;
  }
}

/** POST /api/auth/cli-device/authorize — authorize a device code entered by the user. */
export async function authorizeDeviceCode(
  vaiToken: string,
  code: string
): Promise<{ ok: boolean; status: number }> {
  try {
    const res = await fetch(`${VAI_SERVER}/api/auth/cli-device/authorize`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${vaiToken}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ code }),
    });
    return { ok: res.ok, status: res.status };
  } catch {
    return { ok: false, status: 0 };
  }
}
