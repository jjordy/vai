import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { useSession } from "~/lib/auth-client";
import { exchangeSession, authorizeDeviceCode } from "~/lib/vai-api";

export const Route = createFileRoute("/cli")({
  component: CliPage,
});

type PageState = "idle" | "submitting" | "success" | "error";

function CliPage() {
  const { data: session, isPending } = useSession();
  const [code, setCode] = useState("");
  const [pageState, setPageState] = useState<PageState>("idle");
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  // Fix #313: render success confirmation before showing the form
  if (pageState === "success") {
    return (
      <div style={{ maxWidth: 480, margin: "100px auto", padding: 24, textAlign: "center" }}>
        <div style={{ fontSize: 48, marginBottom: 16 }}>✓</div>
        <h2 style={{ marginBottom: 8 }}>CLI authorized</h2>
        <p style={{ color: "#6b7280" }}>
          Terminal unblocks within one poll interval. You can close this tab.
        </p>
      </div>
    );
  }

  if (isPending) {
    return <div style={{ padding: 40 }}>Loading…</div>;
  }

  if (!session) {
    return (
      <div style={{ maxWidth: 480, margin: "100px auto", padding: 24 }}>
        <h2 style={{ marginBottom: 8 }}>Sign in required</h2>
        <p style={{ color: "#6b7280" }}>
          <a href="/login">Log in</a> to authorize your CLI.
        </p>
      </div>
    );
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    setPageState("submitting");
    setErrorMsg(null);

    const vaiToken = await exchangeSession(session!.session.token);
    if (!vaiToken) {
      setPageState("error");
      setErrorMsg("Could not authenticate with the vai server. Please try again.");
      return;
    }

    const trimmedCode = code.trim().toUpperCase();
    const result = await authorizeDeviceCode(vaiToken, trimmedCode);

    if (result.ok) {
      // Fix #313: flip to success state — clear form, show confirmation
      setCode("");
      setPageState("success");
    } else if (result.status === 404) {
      setPageState("error");
      setErrorMsg("Code not recognised. Check your terminal and try again.");
    } else {
      setPageState("error");
      setErrorMsg("Something went wrong. Please try again.");
    }
  }

  const codeLength = code.replace("-", "").length;

  return (
    <div style={{ maxWidth: 480, margin: "100px auto", padding: 24 }}>
      <h2 style={{ marginBottom: 8 }}>Authorize CLI</h2>
      <p style={{ color: "#6b7280", marginBottom: 24 }}>
        Enter the code shown in your terminal (e.g.{" "}
        <code style={{ fontFamily: "monospace" }}>ABCD-1234</code>).
      </p>
      <form onSubmit={handleSubmit}>
        <input
          type="text"
          value={code}
          onChange={(e) => {
            setErrorMsg(null);
            if (pageState === "error") setPageState("idle");
            setCode(e.target.value);
          }}
          placeholder="ABCD-1234"
          maxLength={9}
          style={{
            fontFamily: "monospace",
            fontSize: 22,
            letterSpacing: 3,
            padding: "10px 14px",
            textTransform: "uppercase",
          }}
          autoComplete="off"
          autoCapitalize="characters"
          required
        />
        {errorMsg && (
          <p style={{ color: "#dc2626", fontSize: 14, marginTop: 0 }}>{errorMsg}</p>
        )}
        <button
          type="submit"
          disabled={pageState === "submitting" || codeLength < 8}
          style={{ marginTop: 4, width: "100%" }}
        >
          {pageState === "submitting" ? "Authorizing…" : "Authorize"}
        </button>
      </form>
    </div>
  );
}
