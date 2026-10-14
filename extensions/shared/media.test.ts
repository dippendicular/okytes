import { describe, it, expect } from "vitest";
import { mediaSource } from "./capture";
describe("explicit media handoff",()=>{
  it("routes reviewed manifests without replacing their HTTP session origin",()=>{
    expect(mediaSource("https://example.test/video.m3u8?token=123")).toBe("hls+https://example.test/video.m3u8?token=123");
    expect(mediaSource("https://example.test/video.mpd")).toBe("dash+https://example.test/video.mpd");
    expect(()=>mediaSource("https://user:password@example.test/video.m3u8")).toThrow();
    expect(()=>mediaSource("https://example.test/file.zip")).toThrow();
  });
});
