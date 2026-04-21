import { createFileRoute } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { useSession } from "~/lib/auth-client";
import { exchangeSession, listKeys, listRepos, listIssues } from "~/lib/vai-api";

export const Route = createFileRoute("/welcome")({
  component: WelcomePage,
});

type StepStatus = "pending" | "current" | "complete";

interface OnboardingState {
  step1: StepStatus;
  step2: StepStatus;
  step3: StepStatus;
  step4: StepStatus;
  step5: StepStatus;
}

/** Derive all step statuses from the vai server state.
 *
 *  Step 1 (Install CLI): no server signal — derives from step 2.
 *    - fresh user (step 2 pending) → "current"  (fix #312: never "complete" on signup)
 *    - step 2 complete → "complete"
 *
 *  Step 2 (Log in from CLI): complete when any key has agent_type === "cli".
 *    Matches both "CLI on <hostname>" (browser flow) and "CLI (device code)"
 *    (device flow). Must NOT match on name prefix alone (fix #311).
 *
 *  Step 3 (Initialize a repo): complete when GET /api/repos returns ≥1 repo.
 *  Step 4 (Generate agent loop): cannot be detected server-side in v1;
 *    treated as "current" once step 3 is done, auto-complete when step 5 done.
 *  Step 5 (Create first issue): complete when GET /api/issues returns ≥1 issue.
 */
function deriveSteps(
  hasCLIKey: boolean,
  hasRepo: boolean,
  hasIssue: boolean
): OnboardingState {
  const step2: StepStatus = hasCLIKey ? "complete" : "pending";

  // Step 1 derives from step 2 — never independently "complete" (fix #312)
  const step1: StepStatus = step2 === "complete" ? "complete" : "current";

  const step3: StepStatus =
    step2 === "complete" ? (hasRepo ? "complete" : "current") : "pending";

  const step5: StepStatus =
    step3 === "complete" ? (hasIssue ? "complete" : "pending") : "pending";

  // Step 4: can't detect; treat as current once step 3 done, complete when step 5 done
  const step4: StepStatus =
    step3 === "complete"
      ? step5 === "complete"
        ? "complete"
        : "current"
      : "pending";

  // Step 5 becomes "current" once step 4 is reachable
  const step5Final: StepStatus =
    step4 === "current" || step4 === "complete"
      ? step5
      : "pending";

  return { step1, step2, step3, step4, step5: step5Final };
}

function WelcomePage() {
  const { data: session, isPending } = useSession();
  const [vaiToken, setVaiToken] = useState<string | null>(null);
  const [steps, setSteps] = useState<OnboardingState>({
    step1: "current",
    step2: "pending",
    step3: "pending",
    step4: "pending",
    step5: "pending",
  });
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Exchange Better Auth session token for a vai JWT once on mount
  useEffect(() => {
    if (!session?.session?.token) return;
    exchangeSession(session.session.token).then(setVaiToken);
  }, [session?.session?.token]);

  // Poll vai API every 3 seconds while the page is open
  useEffect(() => {
    if (!vaiToken) return;

    async function poll() {
      const [keys, repos, issues] = await Promise.all([
        listKeys(vaiToken!),
        listRepos(vaiToken!),
        listIssues(vaiToken!),
      ]);

      // Fix #311: check agent_type === "cli", not name prefix
      const hasCLIKey = keys.some((k) => k.agent_type === "cli");
      const hasRepo = repos.length > 0;
      const hasIssue = issues.length > 0;

      setSteps(deriveSteps(hasCLIKey, hasRepo, hasIssue));
    }

    poll();
    pollRef.current = setInterval(poll, 3000);
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [vaiToken]);

  if (isPending) {
    return <div style={{ padding: 40 }}>Loading…</div>;
  }

  if (!session) {
    if (typeof window !== "undefined") {
      window.location.href = "/login?next=/welcome";
    }
    return null;
  }

  const serverUrl = import.meta.env.VITE_VAI_SERVER_URL as string;

  return (
    <div style={{ maxWidth: 640, margin: "60px auto", padding: 24 }}>
      <h1 style={{ marginBottom: 8 }}>Welcome to vai</h1>
      <p style={{ color: "#6b7280", marginBottom: 32 }}>
        Get your first agent loop running in about 5 minutes.
      </p>

      <StepCard
        number={1}
        status={steps.step1}
        heading="Install the CLI"
        body={
          <CodeBlock code="curl -fsSL https://vai.dev/install.sh | sh" />
        }
      />

      <StepCard
        number={2}
        status={steps.step2}
        heading="Log in from the CLI"
        body={<CodeBlock code="vai login" />}
      />

      <StepCard
        number={3}
        status={steps.step3}
        heading="Initialize a repo"
        body={<CodeBlock code={"cd ~/my-project\nvai init"} />}
      />

      <StepCard
        number={4}
        status={steps.step4}
        heading="Generate an agent loop"
        body={<CodeBlock code="vai agent loop init" />}
      />

      <StepCard
        number={5}
        status={steps.step5}
        heading="Create your first issue"
        body={
          <p style={{ fontSize: 14, color: "#374151" }}>
            Open the{" "}
            <a href={`${serverUrl}/issues/new`} target="_blank" rel="noreferrer">
              issue creator
            </a>{" "}
            in the dashboard to create your first issue.
          </p>
        }
      />
    </div>
  );
}

function StepCard({
  number,
  status,
  heading,
  body,
}: {
  number: number;
  status: StepStatus;
  heading: string;
  body: React.ReactNode;
}) {
  const dot =
    status === "complete" ? "✓" : status === "current" ? "⊙" : "○";
  const label =
    status === "complete"
      ? "Complete"
      : status === "current"
        ? "Current"
        : "";
  const opacity = status === "pending" ? 0.45 : 1;

  return (
    <div
      style={{
        border: "1px solid #e5e7eb",
        borderRadius: 12,
        padding: "16px 20px",
        marginBottom: 16,
        opacity,
      }}
    >
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          marginBottom: status === "pending" ? 0 : 10,
        }}
      >
        <strong style={{ fontSize: 15 }}>
          {dot} {number}. {heading}
        </strong>
        {label && (
          <span style={{ fontSize: 13, color: "#6b7280" }}>{label}</span>
        )}
      </div>
      {status !== "pending" && body}
    </div>
  );
}

function CodeBlock({ code }: { code: string }) {
  const [copied, setCopied] = useState(false);

  function copy() {
    navigator.clipboard.writeText(code).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  }

  return (
    <div
      style={{
        background: "#f3f4f6",
        borderRadius: 6,
        padding: "10px 14px",
        display: "flex",
        justifyContent: "space-between",
        alignItems: "flex-start",
        gap: 8,
      }}
    >
      <pre style={{ margin: 0, fontFamily: "monospace", fontSize: 13, whiteSpace: "pre-wrap" }}>
        {code}
      </pre>
      <button
        onClick={copy}
        style={{
          background: "none",
          border: "1px solid #d1d5db",
          borderRadius: 4,
          padding: "2px 8px",
          fontSize: 12,
          cursor: "pointer",
          whiteSpace: "nowrap",
          flexShrink: 0,
        }}
      >
        {copied ? "Copied!" : "Copy"}
      </button>
    </div>
  );
}
