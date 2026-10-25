import { probeQianwenAccount } from "../consumer-web-identity.mjs";
import { consumerWebProviders } from "../consumer-web-providers.mjs";
import { CONNECTOR_VERSION, defineProvider, unsupported } from "./contract.mjs";

export const qianwenAdapter = defineProvider({
  ...consumerWebProviders.qianwen,
  connectorVersion: CONNECTOR_VERSION,
  operations: [],
  identify: probeQianwenAccount,
  async execute() {
    return unsupported("measurement_adapter_unavailable");
  },
});
