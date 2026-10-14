import type { TrafficPolicy } from "../../../crates/core/bindings/TrafficPolicy";
import type { ScheduleWindow } from "../../../crates/core/bindings/ScheduleWindow";

export const defaultTraffic: TrafficPolicy = {
  global_bps: null,
  per_job_bps: null,
  job_limits: [],
  timezone: "UTC",
  queue_allowed: true,
  windows: [],
};
const days = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const time = (minute: number) =>
  `${Math.floor(minute / 60)
    .toString()
    .padStart(2, "0")}:${(minute % 60).toString().padStart(2, "0")}`;
const minute = (value: string) => {
  const [h, m] = value.split(":").map(Number);
  return h * 60 + m;
};
function Rate({
  label,
  value,
  onChange,
}: {
  label: string;
  value: number | null;
  onChange: (value: number | null) => void;
}) {
  return (
    <label>
      {label}
      <input
        type="number"
        min={1}
        max={1073741824}
        placeholder="Unlimited"
        value={value === null ? "" : value / 1024}
        onChange={(e) =>
          onChange(
            e.target.value === ""
              ? null
              : Math.max(1, Math.round(Number(e.target.value) * 1024)),
          )
        }
      />
      <small>KiB/s · leave empty for unlimited</small>
    </label>
  );
}
export function TrafficSettings({
  policy,
  onChange,
}: {
  policy: TrafficPolicy;
  onChange: (policy: TrafficPolicy) => void;
}) {
  const update = (index: number, change: Partial<ScheduleWindow>) =>
    onChange({
      ...policy,
      windows: policy.windows.map((rule, i) =>
        i === index ? { ...rule, ...change } : rule,
      ),
    });
  return (
    <fieldset>
      <legend>Bandwidth and schedules</legend>
      <Rate
        label="Total download limit"
        value={policy.global_bps}
        onChange={(global_bps) => onChange({ ...policy, global_bps })}
      />
      <Rate
        label="Limit for each download"
        value={policy.per_job_bps}
        onChange={(per_job_bps) => onChange({ ...policy, per_job_bps })}
      />
      <label>
        Schedule timezone
        <input
          value={policy.timezone}
          placeholder="Asia/Dhaka"
          required
          onChange={(e) => onChange({ ...policy, timezone: e.target.value })}
        />
      </label>
      <label className="checkbox">
        <input
          type="checkbox"
          checked={policy.queue_allowed}
          onChange={(e) =>
            onChange({ ...policy, queue_allowed: e.target.checked })
          }
        />
        Run downloads outside schedule windows
      </label>
      <p>
        Windows use local time. Overnight windows start on the selected day.
        During overlapping windows, the lower limit applies and a pause takes
        precedence.
      </p>
      {policy.windows.map((rule, index) => (
        <fieldset key={index}>
          <legend>Window {index + 1}</legend>
          <div aria-label={`Days for window ${index + 1}`}>
            {days.map((day, n) => (
              <label className="checkbox" key={day}>
                <input
                  type="checkbox"
                  checked={rule.days.includes(n)}
                  onChange={(e) =>
                    update(index, {
                      days: e.target.checked
                        ? [...rule.days, n].sort()
                        : rule.days.filter((value) => value !== n),
                    })
                  }
                />
                {day}
              </label>
            ))}
          </div>
          <label>
            Start time
            <input
              type="time"
              required
              value={time(rule.start_minute)}
              onChange={(e) =>
                update(index, { start_minute: minute(e.target.value) })
              }
            />
          </label>
          <label>
            End time
            <input
              type="time"
              required
              value={time(rule.end_minute === 1440 ? 0 : rule.end_minute)}
              onChange={(e) =>
                update(index, {
                  end_minute:
                    minute(e.target.value) === 0
                      ? 1440
                      : minute(e.target.value),
                })
              }
            />
          </label>
          <Rate
            label={`Total limit in window ${index + 1}`}
            value={rule.global_bps}
            onChange={(global_bps) => update(index, { global_bps })}
          />
          <label>
            Queue during window
            <select
              value={
                rule.queue_allowed === null
                  ? "unchanged"
                  : rule.queue_allowed
                    ? "run"
                    : "pause"
              }
              onChange={(e) =>
                update(index, {
                  queue_allowed:
                    e.target.value === "unchanged"
                      ? null
                      : e.target.value === "run",
                })
              }
            >
              <option value="unchanged">Keep default</option>
              <option value="run">Run downloads</option>
              <option value="pause">Pause downloads</option>
            </select>
          </label>
          <button
            type="button"
            onClick={() =>
              onChange({
                ...policy,
                windows: policy.windows.filter((_, i) => i !== index),
              })
            }
          >
            Remove window {index + 1}
          </button>
        </fieldset>
      ))}
      <button
        type="button"
        disabled={policy.windows.length >= 32}
        onClick={() =>
          onChange({
            ...policy,
            windows: [
              ...policy.windows,
              {
                days: [0, 1, 2, 3, 4],
                start_minute: 540,
                end_minute: 1020,
                global_bps: null,
                queue_allowed: null,
              },
            ],
          })
        }
      >
        Add schedule window
      </button>
      {policy.job_limits.map((limit, index) => (
        <Rate
          key={limit.id}
          label={`Limit for download ${limit.id}`}
          value={limit.bps}
          onChange={(bps) =>
            onChange({
              ...policy,
              job_limits: policy.job_limits.map((value, i) =>
                i === index ? { ...value, bps } : value,
              ),
            })
          }
        />
      ))}
    </fieldset>
  );
}
