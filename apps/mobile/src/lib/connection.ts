import { EnvironmentId } from "@t3tools/contracts";
import { type EnvironmentConnectionPhase } from "@t3tools/client-runtime/connection";

export interface SavedRemoteConnection {
  readonly environmentId: EnvironmentId;
  readonly environmentLabel: string;
  readonly displayUrl: string;
  readonly httpBaseUrl: string;
  readonly wsBaseUrl: string;
}

export type RemoteClientConnectionState = EnvironmentConnectionPhase;
