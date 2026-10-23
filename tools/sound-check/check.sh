#!/bin/bash
# Plays tones through the plugin's sound output under Wine, which plays
# WASAPI into PulseAudio, records them from a null sink and checks what was
# heard: the WASAPI side, which CI's Windows machines (with no sound device)
# can't run.
#
#   tools/sound-check/check.sh
#
# Needs Rust with the x86_64-pc-windows-gnu target, MinGW-w64, Wine,
# PulseAudio (pulseaudio, pactl, parec) and python3.
set -euo pipefail
cd "$(dirname "$0")"
WORK=$(mktemp -d)
PIDS=()
trap 'kill "${PIDS[@]}" 2>/dev/null; [ -n "${KEEP:-}" ] || rm -rf "$WORK"' EXIT

cargo build --release --target x86_64-pc-windows-gnu
EXE=target/x86_64-pc-windows-gnu/release/sound-check.exe

mkdir -p "$WORK/pulse"
export PULSE_SERVER="unix:$WORK/pulse/native"
XDG_RUNTIME_DIR="$WORK/pulse" HOME="$WORK" pulseaudio --daemonize=no --exit-idle-time=-1 \
    --disallow-exit -n --use-pid-file=no --system=no \
    --load="module-native-protocol-unix auth-anonymous=1 socket=$WORK/pulse/native" \
    --load="module-null-sink sink_name=check" --load="module-always-sink" >"$WORK/pulse.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 50); do pactl info >/dev/null 2>&1 && break; sleep 0.1; done
pactl set-default-sink check

export WINEPREFIX="$WORK/wine" WINEDEBUG=-all WINEDLLOVERRIDES="mscoree,mshtml="
wineboot --init >/dev/null 2>&1 && wineserver --wait
# Once to warm Wine up.
wine "$EXE" steady >/dev/null

fail=0
for mode in steady bursts; do
    parec -d check.monitor --format=float32le --rate=48000 --channels=2 \
        --latency-msec=10 >"$WORK/$mode.raw" &
    rec=$!
    sleep 0.5
    played=$(wine "$EXE" "$mode" 2>/dev/null | tail -1)
    echo "== $mode: $played"
    # The null sink hands its monitor sound in chunks of up to two seconds.
    sleep 3
    kill "$rec"
    wait "$rec" 2>/dev/null || true
    python3 - "$WORK/$mode.raw" "$mode" "$played" <<'PY' || fail=1
import array, math, sys
path, mode, played = sys.argv[1], sys.argv[2], sys.argv[3].split()
frames = int(played[played.index("frames") + 1])
a = array.array("f")
data = open(path, "rb").read()
a.frombytes(data[: len(data) // 8 * 8])
left = a[0::2]
# Tone or silence in 10 ms blocks.
blocks = [math.sqrt(sum(x * x for x in left[b:b + 480]) / 480) > 0.2
          for b in range(0, len(left) - 480, 480)]
first, last = blocks.index(True), len(blocks) - blocks[::-1].index(True)
tone = sum(blocks)
gaps, run = [], 0
for b in blocks[first:last]:
    if b:
        if run:
            gaps.append(run * 10)
        run = 0
    else:
        run += 1
# 440 Hz at 48 kHz: a zero crossing upwards every 109 samples.
ups = [i for i in range(1, len(left)) if left[i - 1] < 0 <= left[i] and abs(left[i]) < 0.1]
steps = [b - a for a, b in zip(ups, ups[1:]) if b - a < 200]
in_tune = sum(1 for s in steps if abs(s - 48000 / 440) <= 2) / max(len(steps), 1)
peak = max(abs(x) for x in left)
print(f"   heard {tone * 10} ms of tone, gaps {gaps} ms, {in_tune:.0%} in tune, peak {peak:.2f}")
spans, start = [], first
for i in range(first + 1, last + 1):
    if i == last or blocks[i] != blocks[i - 1]:
        spans.append(f"{'tone' if blocks[start] else 'gap'} {start / 100:.2f}-{i / 100:.2f}")
        start = i
print(f"   recording {len(blocks) / 100:.2f} s: " + ", ".join(spans))
# Wine's own timing is loose: a new prefix starts its sound late and
# hiccups now and then, so this checks what WASAPI took and that it was
# heard, in tune and at its level, rather than every gap (the buffer's own
# tests cover those).
if mode == "steady":
    # 2.6 s of tone sent; the muting leaves a gap.
    ok = frames >= 100_000 and tone >= 120 and any(g >= 300 for g in gaps)
else:
    # 6 s of tone, without dropouts.
    ok = frames >= 270_000 and tone >= 500 and all(g <= 100 for g in gaps)
ok = ok and in_tune > 0.95 and 0.4 < peak < 0.6
print("   ok" if ok else "   FAILED")
sys.exit(0 if ok else 1)
PY
done
exit $fail
