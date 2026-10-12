// Declared omissions are audit metadata, not proof of sanitization or ownership.
export function sourceRedaction(manifest, document) {
  const invalid = () => {
    throw new Error("observation_source_redaction_invalid");
  };
  if (
    !manifest ||
    typeof manifest !== "object" ||
    Array.isArray(manifest) ||
    Object.keys(manifest).length !== 3 ||
    manifest.schema_version !== "geo.observation.source-redaction.v1" ||
    manifest.policy !== "reasoning-and-private-fields" ||
    !Array.isArray(manifest.omitted_paths) ||
    manifest.omitted_paths.length > 32_768 ||
    !Array.isArray(document.messages)
  )
    invalid();
  for (const path of manifest.omitted_paths) {
    if (
      typeof path !== "string" ||
      Buffer.byteLength(path, "utf8") > 2_048 ||
      /[\u0000-\u001f\u007f-\u009f]/u.test(path) ||
      !/^\/messages\/(?:0|[1-9]\d*)\/(?:[^~]|~[01])*$/u.test(path)
    )
      invalid();
    const parts = path
      .slice(1)
      .split("/")
      .map((part) => part.replace(/~1/gu, "/").replace(/~0/gu, "~"));
    const index = Number(parts[1]);
    if (!Number.isSafeInteger(index) || index >= document.messages.length)
      invalid();
    let value = document;
    let exists = true;
    for (const part of parts) {
      if (
        value === null ||
        typeof value !== "object" ||
        (Array.isArray(value) && !/^(?:0|[1-9]\d*)$/u.test(part)) ||
        !Object.hasOwn(value, part)
      ) {
        exists = false;
        break;
      }
      value = value[part];
    }
    if (exists) invalid();
  }
  return {
    schema_version: manifest.schema_version,
    policy: manifest.policy,
    omitted_paths: [...manifest.omitted_paths],
  };
}
