import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, it, expect, vi } from "vitest";
import { FileTransferForm, MediaForm } from "./Transfers";
import { api } from "./api";
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
vi.mock("./api", () => ({
  api: {
    submit: vi.fn().mockResolvedValue("job"),
    authorizeFile: vi.fn().mockResolvedValue(undefined),
    control: vi.fn().mockResolvedValue(undefined),
    inspectMedia: vi.fn().mockResolvedValue([
      {
        id: "v",
        label: "1080p",
        url: "hls+https://example.test/video.m3u8",
        kind: "video",
        extension: "ts",
      },
      {
        id: "a",
        label: "English",
        url: "hls+https://example.test/audio.m3u8",
        kind: "audio",
        extension: "mp4",
      },
    ]),
  },
}));
describe("protocol forms", () => {
  it("submits selected media tracks independently", async () => {
    const user = userEvent.setup();
    const done = vi.fn();
    render(<MediaForm destination="D:/Downloads" done={done} />);
    await user.type(
      screen.getByLabelText("Manifest URL"),
      "https://example.test/master.m3u8",
    );
    await user.click(screen.getByRole("button", { name: "Inspect tracks" }));
    expect(api.inspectMedia).toHaveBeenCalledWith(
      "hls+https://example.test/master.m3u8",
    );
    await user.click(await screen.findByLabelText("audio: English"));
    await user.click(
      screen.getByRole("button", { name: "Download selected tracks" }),
    );
    await waitFor(() => expect(done).toHaveBeenCalled());
    expect(api.submit).toHaveBeenCalledWith(
      expect.objectContaining({
        url: "hls+https://example.test/audio.m3u8",
        credentials: null,
      }),
    );
  });
  it("requires explicit fingerprint confirmation and restores credentials before resuming", async () => {
    const user = userEvent.setup();
    const done = vi.fn();
    render(
      <FileTransferForm
        id="file-job"
        protocol="sftp"
        destination="D:/Downloads"
        done={done}
      />,
    );
    await user.type(screen.getByLabelText("Username"), "alice");
    await user.type(screen.getByLabelText("Password"), "secret");
    await user.type(
      screen.getByLabelText("SFTP host fingerprint"),
      "SHA256:verified",
    );
    await user.click(screen.getByRole("checkbox"));
    await user.click(
      screen.getByRole("button", { name: "Authorize and resume" }),
    );
    await waitFor(() => expect(done).toHaveBeenCalled());
    expect(api.authorizeFile).toHaveBeenCalledWith("file-job", {
      username: "alice",
      password: "secret",
      confirmed_host_key: "SHA256:verified",
    });
    expect(api.control).toHaveBeenCalledWith("file-job", { kind: "Resume" });
  });
});
