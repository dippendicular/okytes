import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { TrafficSettings, defaultTraffic } from "./TrafficSettings";
describe("bandwidth settings", () => {
  it("edits actual policy fields and creates a recurring local-time window", () => {
    const change = vi.fn();
    const view = render(
      <TrafficSettings policy={defaultTraffic} onChange={change} />,
    );
    fireEvent.change(screen.getByLabelText(/Total download limit/), {
      target: { value: "128" },
    });
    expect(change.mock.lastCall?.[0].global_bps).toBe(131072);
    fireEvent.click(
      screen.getByRole("button", { name: "Add schedule window" }),
    );
    const policy = change.mock.lastCall?.[0];
    expect(policy.windows[0].days).toEqual([0, 1, 2, 3, 4]);
    view.rerender(<TrafficSettings policy={policy} onChange={change} />);
    fireEvent.change(screen.getByLabelText("Queue during window"), {
      target: { value: "pause" },
    });
    expect(change.mock.lastCall?.[0].windows[0].queue_allowed).toBe(false);
    view.unmount();
  });
});
