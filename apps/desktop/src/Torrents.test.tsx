import { afterEach, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { TorrentForm } from "./Torrents";
import { api } from "./api";
vi.mock("./api", () => ({
  bytes: (n: number) => `${n} B`,
  api: { inspectTorrent: vi.fn(), addTorrent: vi.fn(), torrent: vi.fn() },
}));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
it("previews selected files and submits explicit discovery and seeding options", async () => {
  vi.mocked(api.inspectTorrent).mockResolvedValue({
    info_hash: "a".repeat(40),
    name: "generated",
    files: [
      { index: 0, path: "chosen.bin", bytes: 100, selected: true },
      { index: 1, path: "other.bin", bytes: 200, selected: true },
    ],
    pieces: 1,
    piece_bytes: 16384,
    trackers: [],
  });
  vi.mocked(api.addTorrent).mockResolvedValue("job");
  const done = vi.fn();
  render(<TorrentForm destination="C:\\Downloads" done={done} />);
  fireEvent.change(screen.getByLabelText("Magnet link"), {
    target: { value: `magnet:?xt=urn:btih:${"a".repeat(40)}` },
  });
  fireEvent.click(screen.getByRole("button", { name: "Inspect files" }));
  await screen.findByLabelText(/other.bin/);
  fireEvent.click(screen.getByLabelText(/other.bin/));
  fireEvent.click(screen.getByRole("button", { name: "Add torrent" }));
  await waitFor(() => expect(done).toHaveBeenCalled());
  expect(vi.mocked(api.addTorrent).mock.lastCall?.[0].options).toMatchObject({
    files: [0],
    seed_seconds: 0,
    privacy: { dht: false, pex: false, local_discovery: false },
  });
});
