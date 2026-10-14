import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { Updates } from "./Updates";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
beforeEach(() => vi.mocked(invoke).mockReset());
it("makes no startup request and requires separate download and install actions", async () => {
  vi.mocked(invoke)
    .mockResolvedValueOnce({ version: "0.2.0", notes: "Recovery fixes" })
    .mockResolvedValue(undefined);
  render(<Updates />);
  expect(invoke).not.toHaveBeenCalled();
  fireEvent.click(screen.getByText("Check for updates"));
  await screen.findByText("Okytes 0.2.0");
  expect(screen.getByText("Recovery fixes")).toBeTruthy();
  expect(screen.queryByText("Install verified update")).toBeNull();
  fireEvent.click(screen.getByText("Download and verify update"));
  await screen.findByText("Install verified update");
  expect(invoke).toHaveBeenCalledWith("download_update", { confirmed: true });
  expect(invoke).not.toHaveBeenCalledWith("install_update", expect.anything());
  fireEvent.click(screen.getByText("Install verified update"));
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("install_update", { confirmed: true }),
  );
});
