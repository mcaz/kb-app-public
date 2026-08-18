import { describe, expect, it } from "vitest";

import { queryDefaults } from "./queryClient";

describe("query defaults", () => {
  it("does not refetch every active screen query when the window regains focus", () => {
    expect(queryDefaults.queries?.refetchOnWindowFocus).toBe(false);
  });
});
