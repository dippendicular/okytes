import { it, expect } from "vitest";
import { checkedReply, ownPage } from "./messages";
it("allows extension pages in tabs but rejects content and external senders", () => {
  const prefix = "chrome-extension://abcdef/";
  expect(
    ownPage({ id: "abcdef", url: `${prefix}popup.html` }, "abcdef", prefix),
  ).toBe(true);
  expect(
    ownPage({ id: "abcdef", url: "https://example.com" }, "abcdef", prefix),
  ).toBe(false);
  expect(
    ownPage({ id: "other", url: `${prefix}popup.html` }, "abcdef", prefix),
  ).toBe(false);
});
it("accepts a successful response with null error and exposes real failure", () => {
  const response = { version: 1, ok: true, error: null };
  expect(checkedReply(response)).toBe(response);
  expect(() => checkedReply({ error: "app_unavailable" })).toThrow(
    "app_unavailable",
  );
});
