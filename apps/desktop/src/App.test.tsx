import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import axe from "axe-core";
import { App } from "./App";
import { api, bytes, category, parseUrls } from "./api";

vi.mock("./api", async (original) => {
  const actual = await original<typeof import("./api")>();
  return {
    ...actual,
    api: {
      list: vi.fn(),
      settings: vi.fn(),
      add: vi.fn(),
      control: vi.fn(),
      setSettings: vi.fn(),
      pauseAll: vi.fn(),
    },
  };
});
beforeEach(() => {
  // jsdom has no font/canvas metrics; the native WebView test runs axe with
  // actual rendering and contrast checks enabled.
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(
    () => null,
  );
  vi.stubGlobal("matchMedia", () => ({
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  }));
  vi.mocked(api.list).mockResolvedValue({
    revision: "1",
    jobs: [],
    next: null,
  });
  vi.mocked(api.settings).mockResolvedValue({
    version: 1,
    destination: "C:\\Downloads",
    concurrency: 4,
    theme: "system",
    notifications: true,
  });
  vi.mocked(api.add).mockResolvedValue(["new-id"]);
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.restoreAllMocks();
});
describe("download dashboard", () => {
  it("virtualizes 500 real-shaped jobs, keeps populated rows accessible and sends one bulk pause", async () => {
    vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(400);
    vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(1000);
    const jobs = Array.from({ length: 500 }, (_, index) => ({
      id: String(index),
      name: `file-${index}.zip`,
      url: `https://example.com/file-${index}.zip`,
      destination: `C:\\Downloads\\file-${index}.zip`,
      state: "Queued",
      bytes: 0,
      total: null,
      speed: 0,
      priority: 0,
      retries: 0,
      workers: 0,
      error: null,
      etag: null,
      response_status: null,
      response_latency_ms: null,
      segments: [],
    }));
    vi.mocked(api.list).mockImplementation(async (offset = 0) => ({
      revision: "1",
      jobs: jobs.slice(offset, offset + 256),
      next: offset + 256 < 500 ? offset + 256 : null,
    }));
    vi.mocked(api.pauseAll).mockResolvedValue();
    const { container } = render(<App />);
    await screen.findByRole("button", { name: "file-0.zip" });
    expect(screen.getAllByRole("listitem").length).toBeLessThan(20);
    const result = await axe.run(container, {
      rules: { "color-contrast": { enabled: false } },
    });
    expect(result.violations).toEqual([]);
    fireEvent.click(screen.getByRole("button", { name: "Pause all" }));
    await vi.waitFor(() => expect(api.pauseAll).toHaveBeenCalledTimes(1));
    expect(api.control).not.toHaveBeenCalled();
  });
  it("main view has labels and no axe violations", async () => {
    const { container } = render(<App />);
    await screen.findByText("Your downloads belong here");
    const results = await axe.run(container, {
      rules: { "color-contrast": { enabled: false } },
    });
    expect(results.violations).toEqual([]);
  });
  it("add dialog submits validated batch shape to the real bridge", async () => {
    render(<App />);
    fireEvent.click(screen.getByText("＋ Add downloads"));
    fireEvent.change(screen.getByLabelText("URLs"), {
      target: { value: "https://example.com/a.zip\nhttps://example.com/b.zip" },
    });
    await screen.findByDisplayValue("C:\\Downloads");
    fireEvent.click(screen.getByText("Start downloads"));
    await vi.waitFor(() =>
      expect(api.add).toHaveBeenCalledWith({
        urls: ["https://example.com/a.zip", "https://example.com/b.zip"],
        destination: "C:\\Downloads",
        allow_duplicates: false,
      }),
    );
  });
  it("settings and shortcut dialogs pass accessibility checks", async () => {
    render(<App />);
    await screen.findByText("Your downloads belong here");
    fireEvent.click(screen.getByText("Settings"));
    const results = await axe.run(document.body, {
      rules: { "color-contrast": { enabled: false } },
    });
    expect(results.violations).toEqual([]);
  });
});
it("formatting, categories and batch limits", () => {
  expect(bytes(1024)).toBe("1.0 KiB");
  expect(category("archive.zip")).toBe("Archive");
  expect(parseUrls("https://example.com\n")).toEqual(["https://example.com"]);
  expect(() => parseUrls("x ".repeat(101))).toThrow();
});
