# Steam Input diagnostic: raw changes and meaningful activity

The Windows probe preserves every sampled SDK/XInput value. Its existing
`xinput_state_changes` and `action_state_changes` fields remain raw adjacent
sample-change counts under the same verified controller route/handle. They are
not sufficient evidence of deliberate input: the neutral software-fixture run
[`steam-input-product-fixture-2.json`](../../outputs/steam-input-product-fixture-2.json)
changed a centred Y axis from 0 to 1 without any requested input.

`steam_input_activity_verified` now requires at least one controller and at
least one `xinput_meaningful_changes` or `action_meaningful_changes` event. Each
XInput snapshot includes `meaningful_change`. A summary includes the versioned
`activity_policy` thresholds used to classify the unchanged raw values.

## Policy version 1

- Defined XInput button transitions count, including releases. Reserved button
  bits remain in raw values but do not count as user controls.
- Left and right stick centres use circular dead zones of 7849 and 8689. A
  movement must involve a position outside the relevant zone and travel at
  least 1024 units from that stick's retained activity anchor. Both axes are
  considered together. Returning from a held position counts as movement.
- Triggers require a value above 30 in the current or anchored position and a
  change of at least 4 units. Centre noise and one-unit quantization do not
  count. Press/release and sufficiently large changes while held do count.
- Each stick and trigger retains its own anchor until a meaningful change.
  Slow movement accumulates; button presses do not reset stick anchors. A
  missing route, a different mapped Steam handle or the first observation
  initializes a baseline and does not itself count as activity.

The centre constants follow
[Microsoft's XInput guidance](https://learn.microsoft.com/windows/win32/xinput/getting-started-with-xinput#dead-zone).
The movement deltas are a conservative SkinOS diagnostic policy. They are not a
vendor guarantee that every possible controller's drift will fit these limits,
and a false result does not prove that no small physical movement occurred.

Digital Steam Input actions count actual state transitions while active for the
same controller. Normalized analog modes `JoystickMove` (6), `JoystickCamera`
(8) and `Trigger` (10) have separate retained anchors: stick centre 0.25,
movement delta 0.03125, and trigger thresholds 30/255 and 4/255. Mode changes,
binding availability changes, nonfinite or out-of-range values reset the
baseline. Mouse-like and other analog action modes stay visible in raw samples
and raw counters, but are not classified by this normalized-stick policy.
Valve documents that analog modes can use different units, including relative
mouse deltas; applying one normalized threshold to every mode would be invalid.
[Analog data semantics](https://partner.steamgames.com/doc/api/ISteamInput#InputAnalogActionData_t),
[pinned enum](https://github.com/ValveSoftware/Proton/blob/164e0ccd2ea2b1ec2e5d08dc97f65b184c2539ca/lsteamclient/steamworks_sdk_153a/isteaminput.h#L32).

These checks classify observations only. They never filter, replace or synthesize
game input, force a Steam API result, or turn a software fixture into physical
controller proof. `physical_device_verified` remains false. Recorded genuine
software-fixture A/LX press and release should retain two meaningful changes;
the neutral 0-to-1 centre transition should retain one raw change and zero
meaningful changes.

## Offline regression check

`python3 scripts/test-input-probe-activity.py` extracts the production semantic
helpers, compiles a host test, and checks quantization, centre drift, independent
anchors, slow movement, release, route resets and action-mode handling. It also
reclassifies the preserved neutral and genuine activity traces using those same
helpers. This does not rerun either live test or modify their historical reports.
Production probe packaging and a new live diagnostic remain the parent runner's
separate verification step.
