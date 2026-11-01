import { isProviderDriverKind, ProviderDriverKind } from "@t3tools/contracts";

import { modelShortName } from "./providers/modelShortName.ts";
import { defineProviderClient, makeProviderClientRegistry } from "./providers/providerClient.ts";
import { PiSettings } from "./providers/piSettings.ts";

// j3 servers run one harness, omp, which reports itself with Pi's driver kind.
const ompClient = defineProviderClient({
  driverKind: ProviderDriverKind.make("pi"),
  label: "omp",
  acpRegistryAgentIds: [],
  settingsSchema: PiSettings,
  icon: {
    viewBox: "165.29 165.29 469.43 469.43",
    fill: { light: "#0F0F0F", dark: "#F5F5F5" },
    paths: [
      {
        d: "M165.29 165.29H517.36V400H400V517.36H282.65V634.72H165.29ZM282.65 282.65V400H400V282.65Z",
        fillRule: "evenodd",
      },
      { d: "M517.36 400H634.72V634.72H517.36Z" },
    ],
  },
});

export const providerClients = makeProviderClientRegistry([ompClient]);

export function getProviderClient(driver: string | null | undefined) {
  return isProviderDriverKind(driver) ? providerClients.get(driver) : undefined;
}

export function resolveModelShortName(
  model: { readonly name: string; readonly shortName?: string | undefined },
  driverKind: ProviderDriverKind | null | undefined,
): string | undefined {
  if (model.shortName) return model.shortName;
  const families = driverKind ? providerClients.get(driverKind)?.modelFamilies : undefined;
  return modelShortName(model.name, families ?? []);
}
