import { EnvironmentId } from "@t3tools/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Schema from "effect/Schema";

import { BearerConnectionProfile, BearerConnectionRegistration } from "../connection/catalog.ts";
import { BearerConnectionTarget } from "../connection/model.ts";
import {
  ConnectionCatalogDocument,
  EMPTY_CONNECTION_CATALOG_DOCUMENT,
  catalogRoutes,
  setRoutesInCatalog,
  registerConnectionInCatalog,
  removeConnectionFromCatalog,
  setConnectionEnabledInCatalog,
} from "./storageDocument.ts";

const ENVIRONMENT_ID = EnvironmentId.make("environment-1");
const decodeCatalogDocument = Schema.decodeUnknownSync(ConnectionCatalogDocument);

function host(connectionId: string, httpBaseUrl: string, environmentId = ENVIRONMENT_ID) {
  const target = new BearerConnectionTarget({ environmentId, label: "shieldy", connectionId });
  const profile = new BearerConnectionProfile({
    connectionId,
    environmentId,
    label: target.label,
    httpBaseUrl,
    wsBaseUrl: httpBaseUrl.replace(/^http/, "ws"),
  });
  return { target, profile, registration: new BearerConnectionRegistration({ target, profile }) };
}

const TAILNET = host("tailnet", "http://shieldy:3780/");
const LAN = host("lan", "http://192.168.1.10:3780/");

describe("ConnectionCatalogDocument", () => {
  it("saves a host's target and profile and forgets both when it is removed", () => {
    const registered = registerConnectionInCatalog(
      EMPTY_CONNECTION_CATALOG_DOCUMENT,
      TAILNET.registration,
    );

    expect(registered.targets).toEqual([TAILNET.target]);
    expect(registered.profiles).toEqual([TAILNET.profile]);
    expect(removeConnectionFromCatalog(registered, ENVIRONMENT_ID)).toEqual(
      EMPTY_CONNECTION_CATALOG_DOCUMENT,
    );
  });

  it("replaces a host's profile when it is registered again", () => {
    const moved = host("tailnet", "http://shieldy.tail1234.ts.net:3780/");
    const document = registerConnectionInCatalog(
      registerConnectionInCatalog(EMPTY_CONNECTION_CATALOG_DOCUMENT, TAILNET.registration),
      moved.registration,
    );

    expect(document.targets).toEqual([moved.target]);
    expect(document.profiles).toEqual([moved.profile]);
  });

  it("decodes a document written before the disabled list existed", () => {
    const decoded = decodeCatalogDocument({ schemaVersion: 1, targets: [], profiles: [] });

    expect(decoded.disabledEnvironmentIds).toEqual([]);
  });

  it("switches a saved environment off and back on without touching its records", () => {
    const registered = registerConnectionInCatalog(
      EMPTY_CONNECTION_CATALOG_DOCUMENT,
      TAILNET.registration,
    );

    const disabled = setConnectionEnabledInCatalog(registered, ENVIRONMENT_ID, false);
    expect(disabled.disabledEnvironmentIds).toEqual([ENVIRONMENT_ID]);
    expect(disabled.targets).toEqual(registered.targets);
    expect(disabled.profiles).toEqual(registered.profiles);
    expect(
      setConnectionEnabledInCatalog(disabled, ENVIRONMENT_ID, false).disabledEnvironmentIds,
    ).toEqual([ENVIRONMENT_ID]);

    expect(
      setConnectionEnabledInCatalog(disabled, ENVIRONMENT_ID, true).disabledEnvironmentIds,
    ).toEqual([]);
    expect(
      registerConnectionInCatalog(disabled, TAILNET.registration).disabledEnvironmentIds,
    ).toEqual([ENVIRONMENT_ID]);
    expect(removeConnectionFromCatalog(disabled, ENVIRONMENT_ID).disabledEnvironmentIds).toEqual(
      [],
    );
  });

  it("does not disable an environment that is not saved", () => {
    expect(
      setConnectionEnabledInCatalog(EMPTY_CONNECTION_CATALOG_DOCUMENT, ENVIRONMENT_ID, false)
        .disabledEnvironmentIds,
    ).toEqual([]);
  });

  it("keeps every route of an environment and drops only a removed route's records", () => {
    const both = registerConnectionInCatalog(
      registerConnectionInCatalog(EMPTY_CONNECTION_CATALOG_DOCUMENT, TAILNET.registration),
      LAN.registration,
      [TAILNET.target, LAN.target],
    );
    const withProfiles = { ...both, profiles: [TAILNET.profile, LAN.profile] };
    expect(catalogRoutes(withProfiles, ENVIRONMENT_ID)).toEqual([TAILNET.target, LAN.target]);

    const lanOnly = setRoutesInCatalog(withProfiles, ENVIRONMENT_ID, [LAN.target]);
    expect(lanOnly.targets).toEqual([LAN.target]);
    expect(lanOnly.profiles).toEqual([LAN.profile]);
  });

  it("keeps an environment's position in the catalog when its routes change", () => {
    const other = host("other", "http://desk:3780/", EnvironmentId.make("environment-2"));
    const document = {
      ...EMPTY_CONNECTION_CATALOG_DOCUMENT,
      targets: [TAILNET.target, other.target],
    };
    expect(
      setRoutesInCatalog(document, ENVIRONMENT_ID, [LAN.target, TAILNET.target]).targets,
    ).toEqual([LAN.target, TAILNET.target, other.target]);
  });
});
