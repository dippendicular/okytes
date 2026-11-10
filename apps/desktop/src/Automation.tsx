import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, type Automation, type Job } from "./api";

const finishActions: Record<string, [string, string]> = {
  sleep: ["Put the computer to sleep", "The computer goes to sleep"],
  shutdown: ["Shut down the computer", "The computer shuts down"],
  quit: ["Quit Okytes", "Okytes quits"],
};

// Resolved lazily so a shell without these commands only rejects a promise.
function load(): Promise<Automation> {
  return Promise.resolve().then(() => api.automation());
}
function reason(error: unknown): string {
  return String(error).replace(/^Error:\s*/i, "");
}

// Rendered inside the Settings form. Choices apply at once, so nothing here
// submits or depends on saving the surrounding settings.
export function AutomationSettings() {
  const [state, setState] = useState<Automation>();
  const [message, setMessage] = useState("");
  useEffect(() => {
    let live = true;
    load()
      .then((value) => live && setState(value))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);
  if (!state) return null;
  const apply = (task: Promise<void>) => {
    setMessage("");
    void task
      .then(load)
      .then(setState)
      .catch((e: unknown) => setMessage(reason(e)));
  };
  return (
    <fieldset className="settings-section">
      <legend>When downloads finish</legend>
      <label>
        After every download has finished
        <select
          value={state.when_done ?? state.pending ?? ""}
          onChange={(e) => apply(api.setWhenDone(e.target.value || null))}
        >
          <option value="">Do nothing</option>
          {Object.entries(finishActions).map(([value, [label]]) => (
            <option key={value} value={value}>
              {label}
            </option>
          ))}
        </select>
      </label>
      <p className="muted">
        Applies once, and only until Okytes is closed. Okytes waits 60 seconds
        before acting, and you can cancel during that time.
      </p>
      <label className="checkbox">
        <input
          type="checkbox"
          checked={state.extract}
          onChange={(e) => apply(api.setExtract(e.target.checked))}
        />
        Extract zip, 7z and tar archives into a new folder beside the download
      </label>
      {message && (
        <p role="alert" className="alert">
          {message}
        </p>
      )}
    </fieldset>
  );
}

// Main-window notice while a finish action counts down.
export function WhenDoneNotice() {
  const [action, setAction] = useState<string | null>(null);
  useEffect(() => {
    let stop: (() => void) | undefined;
    let live = true;
    listen<string | null>("when-done", (event) => setAction(event.payload))
      .then((unlisten) => {
        if (live) stop = unlisten;
        else unlisten();
      })
      .catch(() => {});
    load()
      .then((value) => live && setAction(value.pending))
      .catch(() => {});
    return () => {
      live = false;
      stop?.();
    };
  }, []);
  if (!action) return null;
  return (
    <div role="alert" className="alert">
      All downloads finished. {finishActions[action]?.[1] ?? "Okytes acts"}{" "}
      within a minute.
      <button
        onClick={() => {
          void api
            .setWhenDone(null)
            .then(() => setAction(null))
            .catch(() => {});
        }}
      >
        Cancel
      </button>
    </div>
  );
}

// One-time start for a single unfinished download, in the details dialog.
export function StartAt({ job }: { job: Job }) {
  const [state, setState] = useState<Automation>();
  const [value, setValue] = useState("");
  const [message, setMessage] = useState("");
  useEffect(() => {
    let live = true;
    load()
      .then((result) => live && setState(result))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);
  if (!state) return null;
  const planned = state.starts.find((start) => start.id === job.id);
  const schedule = (at: number | null) => {
    setMessage("");
    void api
      .scheduleStart(job.id, at)
      .then(load)
      .then(setState)
      .catch((e: unknown) => setMessage(reason(e)));
  };
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        // A datetime-local value has no zone and is read as local time.
        const at = Date.parse(value);
        if (Number.isNaN(at)) setMessage("Choose a date and time");
        else schedule(Math.floor(at / 1000));
      }}
    >
      <label>
        Start at
        <input
          type="datetime-local"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          required
        />
      </label>
      {planned ? (
        <p className="muted">
          Starts {new Date(planned.at * 1000).toLocaleString()}.{" "}
          <button type="button" onClick={() => schedule(null)}>
            Clear start time
          </button>
        </p>
      ) : (
        <p className="muted">
          The download stays paused until this time. Okytes must be running, and
          it does not wake a sleeping computer.
        </p>
      )}
      <button>Schedule start</button>
      {message && (
        <p role="alert" className="alert">
          {message}
        </p>
      )}
    </form>
  );
}
