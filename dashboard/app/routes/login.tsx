import { createFileRoute, useNavigate, useSearch } from "@tanstack/react-router";
import { useState } from "react";
import { signIn } from "~/lib/auth-client";
import { z } from "zod";

const searchSchema = z.object({
  next: z.string().optional(),
});

export const Route = createFileRoute("/login")({
  validateSearch: searchSchema,
  component: LoginPage,
});

function LoginPage() {
  const navigate = useNavigate();
  const { next } = Route.useSearch();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    setError(null);
    setLoading(true);
    const result = await signIn.email({ email, password });
    setLoading(false);
    if (result.error) {
      setError(result.error.message ?? "Login failed");
    } else {
      if (next) {
        window.location.href = next;
      } else {
        navigate({ to: "/welcome" });
      }
    }
  }

  return (
    <div style={{ maxWidth: 400, margin: "80px auto", padding: 24 }}>
      <h1 style={{ marginBottom: 24 }}>Log in to vai</h1>
      <form onSubmit={handleSubmit}>
        <label>
          Email
          <input
            type="email"
            required
            autoComplete="email"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
          />
        </label>
        <label>
          Password
          <input
            type="password"
            required
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </label>
        {error && (
          <p style={{ color: "#dc2626", fontSize: 14, marginTop: 0 }}>{error}</p>
        )}
        <button type="submit" disabled={loading} style={{ marginTop: 8, width: "100%" }}>
          {loading ? "Signing in…" : "Sign in"}
        </button>
      </form>
      <p style={{ marginTop: 16, fontSize: 14 }}>
        No account? <a href="/signup">Sign up</a>
      </p>
    </div>
  );
}
