import { probeGlmAccount } from "../consumer-web-identity.mjs";
import { consumerWebProviders } from "../consumer-web-providers.mjs";
import { CONNECTOR_VERSION, defineProvider, unsupported } from "./contract.mjs";

export const glmAdapter = defineProvider({
  ...consumerWebProviders.glm,
  connectorVersion: CONNECTOR_VERSION,
  operations: [],
  identify(page) {
    return probeGlmAccount(page);
  },
  async execute() {
    return unsupported("measurement_adapter_unavailable");
  },
});
