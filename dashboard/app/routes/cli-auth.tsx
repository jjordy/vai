import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { z } from "zod";
import { useSession } from "~/lib/auth-client";
import { exchangeSession, createKey } from "~/lib/vai-api";

/** Fix #306: port must use z.coerce.number so the string "49538" from the URL
 *  is coerced to the number 49538 before range validation. Without coerce,
 *  z.number() rejects the string and the router drops the param entirely. */
const searchSchema = z.object({
  port: z.coerce.number().int().min(49152).max(65535),
  state: z.string().min(1),
  hostname: z.string().optional(),
  name: z.string().optional(),
});

export const Route = createFileRoute("/cli-auth")({
  validateSearch: searchSchema,
  component: CliAuthPage,
});

type PageState = "idle" | "authorizing" | "success" | "cancelled" | "error";

function CliAuthPage() {
  const { port, state, hostname, name } = Route.useSearch();
  const { data: session, isPending } = useSession();
  const [pageState, setPageState] = useState<PageState>("idle");
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  if (isPending) {
    return <div style={{ padding: 40 }}>Loading…</div>;
  }

  if (!session) {
    if (typeof window !== "undefined") {
      window.location.href = `/login?next=${encodeURIComponent(
        window.location.pathname + window.location.search
      )}`;
    }
    return null;
  }

  const displayHost = hostname ?? name ?? "your machine";
  const userEmail = session.user.email;

  async function handleAuthorize() {
    setPageState("authorizing");

    const vaiToken = await exchangeSession(session!.session.token);
    if (!vaiToken) {
      setPageState("error");
      setErrorMsg("Could not authenticate with the vai server. Please try again.");
      return;
    }

    const keyName = hostname ? `CLI on ${hostname}` : (name ?? "CLI");
    const result = await createKey(vaiToken, {
      name: keyName,
      role: "write",
      agent_type: "cli",
    });

    if (!result) {
      setPageState("error");
      setErrorMsg("Failed to create API key. Please try again.");
      return;
    }

    // POST the minted key to the CLI's ephemeral localhost callback
    const callbackUrl = `http://127.0.0.1:${port}/callback?state=${encodeURIComponent(state)}`;
    try {
      await fetch(callbackUrl, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          api_key: result.token,
          user_id: session!.user.id,
          user_email: userEmail,
        }),
      });
      setPageState("success");
    } catch {
      setPageState("error");
      setErrorMsg(
        "API key was created but the CLI callback could not be reached. " +
          "The terminal may have timed out — run `vai login` again."
      );
    }
  }

  if (pageState === "success") {
    return (
      <div style={{ maxWidth: 480, margin: "100px auto", padding: 24, textAlign: "center" }}>
        <div style={{ fontSize: 48, marginBottom: 16 }}>✓</div>
        <h2 style={{ marginBottom: 8 }}>Authorized</h2>
        <p style={{ color: "#6b7280" }}>
          You can close this tab and return to your terminal.
        </p>
      </div>
    );
  }

  if (pageState === "cancelled") {
    return (
      <div style={{ maxWidth: 480, margin: "100px auto", padding: 24, textAlign: "center" }}>
        <h2 style={{ marginBottom: 8 }}>Cancelled</h2>
        <p style={{ color: "#6b7280" }}>Authorization was cancelled. You can close this tab.</p>
      </div>
    );
  }

  if (pageState === "error") {
    return (
      <div style={{ maxWidth: 480, margin: "100px auto", padding: 24 }}>
        <h2 style={{ marginBottom: 8 }}>Error</h2>
        <p style={{ color: "#dc2626" }}>{errorMsg}</p>
        <button
          onClick={() => {
            setPageState("idle");
            setErrorMsg(null);
          }}
          style={{ marginTop: 12 }}
        >
          Try again
        </button>
      </div>
    );
  }

  if (!port) {
    return (
      <div style={{ maxWidth: 480, margin: "100px auto", padding: 24 }}>
        <h2>Invalid Request</h2>
        <p style={{ color: "#dc2626" }}>Missing required query parameter: port.</p>
        <p style={{ color: "#6b7280", fontSize: 14 }}>
          Run <code>vai login</code> in your terminal to generate a valid authorization URL.
        </p>
      </div>
    );
  }

  return (
    <div style={{ maxWidth: 480, margin: "100px auto", padding: 24 }}>
      <h2 style={{ marginBottom: 8 }}>Authorize CLI</h2>
      <p style={{ color: "#374151", marginBottom: 24 }}>
        The vai CLI on <strong>{displayHost}</strong> wants to sign in as{" "}
        <strong>{userEmail}</strong>.
      </p>
      <div style={{ display: "flex", gap: 12 }}>
        <button
          onClick={handleAuthorize}
          disabled={pageState === "authorizing"}
          style={{ padding: "10px 24px" }}
        >
          {pageState === "authorizing" ? "Authorizing…" : "Authorize"}
        </button>
        <button
          onClick={() => setPageState("cancelled")}
          disabled={pageState === "authorizing"}
          style={{
            padding: "10px 24px",
            background: "none",
            border: "1px solid #d1d5db",
            borderRadius: 6,
            cursor: "pointer",
          }}
        >
          Cancel
        </button>
      </div>
    </div>
  );
}
