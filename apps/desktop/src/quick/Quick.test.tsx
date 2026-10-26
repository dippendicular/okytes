import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import axe from "axe-core";
const mocks = vi.hoisted(() => ({
  session: vi.fn(),
  logout: vi.fn(),
  list: vi.fn(),
  add: vi.fn(),
  control: vi.fn(),
  events: vi.fn(
    (_changed: unknown, signal: AbortSignal) =>
      new Promise<void>((resolve) =>
        signal.addEventListener("abort", () => resolve(), { once: true }),
      ),
  ),
}));
vi.mock("../../../../dev-tools/sdk/packages/sdk-typescript/src/quick", () => ({
  createQuick: () => mocks,
}));
import { Quick } from "./Quick";
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
it("requires pairing and keeps the accessible approval form independent of a token", async () => {
  mocks.session.mockRejectedValue(new Error("unauthorized"));
  const { container } = render(<Quick />);
  await screen.findByLabelText("Pairing code");
  expect(screen.queryByText("Download links")).toBeNull();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(
    () => null,
  );
  expect(
    (
      await axe.run(container, {
        rules: { "color-contrast": { enabled: false } },
      })
    ).violations,
  ).toEqual([]);
  vi.restoreAllMocks();
});
it("submits explicit duplicate approval through the shared client and shows real empty state", async () => {
  mocks.session.mockResolvedValue(undefined);
  mocks.list.mockResolvedValue({ revision: "1", jobs: [], next: null });
  mocks.add.mockResolvedValue(["job"]);
  render(<Quick />);
  await screen.findByText("No downloads yet. Paste a link to get started.");
  fireEvent.change(screen.getByLabelText("Download links"), {
    target: { value: "https://example.invalid/file" },
  });
  fireEvent.click(
    screen.getByLabelText("Allow another copy of URLs already in my queue"),
  );
  fireEvent.click(screen.getByRole("button", { name: "Download" }));
  await vi.waitFor(() =>
    expect(mocks.add).toHaveBeenCalledWith({
      urls: ["https://example.invalid/file"],
      destination: null,
      allow_duplicates: true,
    }),
  );
});
