import { consumerWebProviders } from "../consumer-web-providers.mjs";
import { CONNECTOR_VERSION, defineProvider, unsupported } from "./contract.mjs";

export const yuanbaoAdapter = defineProvider({
  ...consumerWebProviders.yuanbao,
  connectorVersion: CONNECTOR_VERSION,
  operations: [],
  async identify() {
    return null;
  },
  async execute() {
    return unsupported("measurement_adapter_unavailable");
  },
});
