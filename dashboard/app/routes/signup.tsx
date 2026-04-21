import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { signUp } from "~/lib/auth-client";

export const Route = createFileRoute("/signup")({
  component: SignupPage,
});

/** Signup form — name, email, and password only. No vaiApiKey field (issue #315). */
function SignupPage() {
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    setError(null);
    setLoading(true);
    const result = await signUp.email({
      name,
      email,
      password,
      callbackURL: "/welcome",
    });
    setLoading(false);
    if (result.error) {
      setError(result.error.message ?? "Signup failed");
    } else {
      navigate({ to: "/welcome" });
    }
  }

  return (
    <div style={{ maxWidth: 400, margin: "80px auto", padding: 24 }}>
      <h1 style={{ marginBottom: 24 }}>Create your vai account</h1>
      <form onSubmit={handleSubmit}>
        <label>
          Name
          <input
            type="text"
            required
            autoComplete="name"
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
        </label>
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
            minLength={8}
            autoComplete="new-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </label>
        {/* No vaiApiKey field — API keys are minted via `vai login` after signup */}
        {error && (
          <p style={{ color: "#dc2626", fontSize: 14, marginTop: 0 }}>{error}</p>
        )}
        <button type="submit" disabled={loading} style={{ marginTop: 8, width: "100%" }}>
          {loading ? "Creating account…" : "Sign up"}
        </button>
      </form>
      <p style={{ marginTop: 16, fontSize: 14 }}>
        Already have an account? <a href="/login">Log in</a>
      </p>
    </div>
  );
}
