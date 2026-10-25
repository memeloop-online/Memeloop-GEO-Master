import { probeDoubaoAccount } from "../consumer-web-identity.mjs";
import { consumerWebProviders } from "../consumer-web-providers.mjs";
import { CONNECTOR_VERSION, defineProvider, unsupported } from "./contract.mjs";

export const doubaoAdapter = defineProvider({
  ...consumerWebProviders.doubao,
  connectorVersion: CONNECTOR_VERSION,
  operations: [],
  identify: probeDoubaoAccount,
  async execute() {
    return unsupported("measurement_adapter_unavailable");
  },
});
