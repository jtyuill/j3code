import { describe, expect, it } from "@effect/vitest";

import { connectionDiagnosisText, presentConnectionDiagnosis } from "./diagnosis.ts";
import {
  AVAILABLE_CONNECTION_STATE,
  ConnectionBlockedError,
  type ConnectionAttemptError,
  ConnectionTransientError,
  type SupervisorConnectionState,
} from "./model.ts";

const backoff = (lastFailure: ConnectionAttemptError): SupervisorConnectionState => ({
  ...AVAILABLE_CONNECTION_STATE,
  desired: true,
  network: "online",
  phase: "backoff",
  attempt: 3,
  lastFailure,
  retryAt: Date.UTC(2026, 9, 19, 12, 0, 30),
});

const diagnose = (state: SupervisorConnectionState) =>
  presentConnectionDiagnosis({ hostLabel: "shieldy", state });

const results = (state: SupervisorConnectionState) =>
  diagnose(state)?.hops.map((hop) => `${hop.hop}:${hop.result}`);

describe("presentConnectionDiagnosis", () => {
  it("returns null while nothing has failed", () => {
    expect(diagnose(AVAILABLE_CONNECTION_STATE)).toBeNull();
  });

  it("blames the host when it does not answer", () => {
    const state = backoff(
      new ConnectionTransientError({ reason: "network", detail: "Failed to fetch", hop: "server" }),
    );
    expect(diagnose(state)?.headline).toBe("j3 on shieldy did not answer");
    expect(results(state)).toEqual(["device:ok", "server:failed"]);
  });

  it("says the host refused this device on an authentication failure", () => {
    const state = backoff(
      new ConnectionBlockedError({ reason: "authentication", detail: "Refused.", hop: "server" }),
    );
    expect(diagnose(state)?.headline).toBe("j3 on shieldy did not accept this device");
  });

  it("asks for a server update when the host is too old", () => {
    const state = backoff(
      new ConnectionBlockedError({
        reason: "configuration",
        detail: "Update the server.",
        serverUpdateRequired: true,
      }),
    );
    expect(diagnose(state)?.headline).toBe("shieldy runs an older j3 server");
    expect(diagnose(state)?.steps).toEqual(["Update j3-server on shieldy."]);
  });

  it("reports this device as offline before any other hop", () => {
    const state = {
      ...backoff(new ConnectionTransientError({ reason: "network", detail: "Offline." })),
      network: "offline" as const,
    };
    expect(diagnose(state)?.headline).toBe("This device is offline");
    expect(results(state)).toEqual(["device:failed", "server:unchecked"]);
  });

  it("formats a plain-text report with the failure facts", () => {
    const state = backoff(
      new ConnectionTransientError({
        reason: "timeout",
        detail: "Timed out.",
        traceId: "trace-1",
        hop: "server",
      }),
    );
    const text = connectionDiagnosisText("shieldy", diagnose(state)!);
    expect(text).toContain("j3 connection to shieldy: j3 on shieldy did not answer");
    expect(text).toContain("j3 on shieldy: FAILED");
    expect(text).toContain("Trace ID: trace-1");
  });
});
