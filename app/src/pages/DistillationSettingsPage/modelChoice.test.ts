import { describe, expect, it } from "vitest";

import { modelChoice } from "./modelChoice";

describe("saved model selection", () => {
  const models = [{ model: "gpt-6-astra" }];

  it("preserves custom and removed model identifiers while the catalog is unavailable", () => {
    expect(modelChoice("gpt-6-astra", [])).toBe("custom");
    expect(modelChoice("my-model", models)).toBe("custom");
    expect(modelChoice(null, models)).toBe("default");
  });

  it("uses catalog models without interrupting an explicit custom edit", () => {
    expect(modelChoice("gpt-6-astra", models)).toBe("model:gpt-6-astra");
    expect(modelChoice("gpt-6-astra", models, true)).toBe("custom");
  });
});
