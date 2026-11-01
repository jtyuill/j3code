import type { AuthClientPresentationMetadata } from "@t3tools/contracts";
import * as Context from "effect/Context";

export class ClientPresentation extends Context.Service<
  ClientPresentation,
  {
    readonly metadata: AuthClientPresentationMetadata;
  }
>()("@t3tools/client-runtime/platform/capabilities/ClientPresentation") {}
