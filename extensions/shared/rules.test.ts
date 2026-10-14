import { describe, it, expect } from "vitest";
import { defaults, normalizeRules, shouldCapture, validUrl } from "./rules";
describe("capture rules", () => {
  const input = {
    url: "https://downloads.example.com/file.zip",
    filename: "file.zip",
    size: 1024,
    bypass: false,
  };
  it("requires opt-in and lets deny override allow with domain boundaries", () => {
    expect(shouldCapture(defaults, input)).toBe(false);
    const rules = normalizeRules({
      ...defaults,
      enabled: true,
      allow: ["example.com"],
      deny: ["blocked.example.com"],
    });
    expect(shouldCapture(rules, input)).toBe(true);
    expect(
      shouldCapture(rules, {
        ...input,
        url: "https://notexample.com/file.zip",
      }),
    ).toBe(false);
    expect(
      shouldCapture(rules, {
        ...input,
        url: "https://blocked.example.com/file.zip",
      }),
    ).toBe(false);
  });
  it("honors type, size, unknown-size and modifier bypass rules", () => {
    const rules = {
      ...defaults,
      enabled: true,
      types: ["zip"],
      minBytes: 512,
      maxBytes: 2048,
    };
    expect(shouldCapture(rules, input)).toBe(true);
    for (const override of [
      { bypass: true },
      { filename: "file.pdf" },
      { size: 400 },
      { size: 4096 },
      { size: -1 },
    ])
      expect(shouldCapture(rules, { ...input, ...override })).toBe(false);
    expect(
      shouldCapture({ ...defaults, enabled: true }, { ...input, size: -1 }),
    ).toBe(true);
  });
  it("rejects credentials, non-HTTP URLs and unsafe settings", () => {
    for (const url of [
      "file:///a",
      "javascript:alert(1)",
      "https://user:password@example.com/a",
    ])
      expect(validUrl(url)).toBe(false);
    for (const values of [
      { allow: ["*.example.com"] },
      { minBytes: -1 },
      { minBytes: 1024, maxBytes: 512 },
      { types: ["zip/../"] },
      { version: 2 },
    ])
      expect(() => normalizeRules({ ...defaults, ...values })).toThrow();
  });
});
