#!/usr/bin/env python3
"""Offline tests of actual extracted input-probe semantic activity helpers.

Only builds a host C++ test under target/input-probe-activity-tests. Recorded
Steam traces are read, never replayed into Steam/Wine or treated as physical
controller evidence. Does not build or modify the production probe.
"""
import hashlib
import json
import pathlib
import re
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE = ROOT / "third_party/notproton/integration/input-probe.cpp"
OUT = ROOT / "target/input-probe-activity-tests"
TRACES = (
    ("neutral_rounding", ROOT / "outputs/steam-input-product-fixture-2.json", 1, 0),
    ("pressed_released", ROOT / "outputs/steam-input-wine-activity-2/steam-input-proof.json", 2, 2),
)


def extract(source):
    begin, end = "// BEGIN SEMANTIC_ACTIVITY", "// END SEMANTIC_ACTIVITY"
    if source.count(begin) != 1 or source.count(end) != 1:
        raise RuntimeError("Expected exactly one production semantic activity block")
    block = source.split(begin, 1)[1].split(end, 1)[0]
    structures = {}
    for name in ("GamepadState", "DigitalData", "AnalogData"):
        match = re.search(r"struct\s+" + name + r"\s*\{[^{}]*\};", source)
        if not match:
            raise RuntimeError("Missing actual structure " + name)
        structures[name] = match.group()
    same = re.search(r"static bool same_pad\([^{}]+\)\s*\{[^{}]*\}", source)
    if not same:
        raise RuntimeError("Missing actual raw pad comparator")
    declarations = structures["GamepadState"] + "\n#pragma pack(push, 1)\n"
    declarations += structures["DigitalData"] + "\n" + structures["AnalogData"] + "\n#pragma pack(pop)\n"
    return declarations + same.group() + "\n" + block


PRELUDE = r'''
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cmath>
#include <limits>
// Windows unsigned long is 32-bit; the host ABI may use a 64-bit long.
using U32 = std::uint32_t;
using U64 = std::uint64_t;
#define CHECK(expr) do { if (!(expr)) { std::fprintf(stderr,"FAIL %s:%d: %s\n",__func__,__LINE__,#expr); std::exit(1); } } while(0)
'''

TESTS = r'''
static_assert(sizeof(GamepadState)==12, "original XInput pad layout");
static_assert(sizeof(AnalogData)==13, "original packed analog action layout");
static unsigned cases;
static void passed(const char *label) { ++cases; std::printf("PASS %s\n",label); }
static bool observe(SemanticPadTracker &t,const GamepadState &p) {return observe_semantic_pad(t,true,42,p);}

static void neutral_rounding() {
    SemanticPadTracker t{};GamepadState p{},before=p;
    CHECK(!observe(t,p));p.left_y=1;p.right_y=1;
    CHECK(!same_pad(before,p));CHECK(!observe(t,p));
    CHECK(t.anchor.left_y==0&&t.anchor.right_y==0);
    passed("neutral Y0-to-1: raw change only, retained semantic anchor");
}
static void drift_inside_deadzones() {
    SemanticPadTracker t{};GamepadState p{};CHECK(!observe(t,p));
    for(int i=1;i<=6;++i){p.left_x=(short)(i*600);p.left_y=(short)(-i*500);
        p.right_x=(short)(i*700);p.right_y=(short)(i*300);p.left_trigger=(unsigned char)(i*4);
        CHECK(!observe(t,p));}
    passed("ordinary stick/trigger drift inside deadzones ignored");
}
static void buttons() {
    SemanticPadTracker t{};GamepadState p{};CHECK(!observe(t,p));
    p.buttons=0x1000;CHECK(observe(t,p));CHECK(!observe(t,p));
    p.buttons=0;CHECK(observe(t,p));
    p.buttons=0x0c00;CHECK(!observe(t,p));p.buttons=0;CHECK(!observe(t,p));
    passed("A press/release significant; reserved/nonstandard bits raw only");
}
static void stick_press_release() {
    for(unsigned stick=0;stick<2;++stick){SemanticPadTracker t{};GamepadState p{};CHECK(!observe(t,p));
        if(stick)p.right_x=16000;else p.left_x=16000;CHECK(observe(t,p));CHECK(!observe(t,p));
        p.left_x=p.right_x=0;CHECK(observe(t,p));}
    passed("left/right stick 0-to-16000 and release significant");
}
static void slow_accumulation() {
    SemanticPadTracker t{};GamepadState p{};p.left_x=12000;CHECK(!observe(t,p));
    for(int x=12200;x<=13000;x+=200){p.left_x=(short)x;CHECK(!observe(t,p));CHECK(t.anchor.left_x==12000);}
    p.left_x=13200;CHECK(observe(t,p));CHECK(t.anchor.left_x==13200);
    passed("retained anchor detects accumulated movement across small samples");
}
static void independent_anchors() {
    SemanticPadTracker t{};GamepadState p{};p.left_x=12000;CHECK(!observe(t,p));
    p.left_x=12300;p.buttons=0x1000;CHECK(observe(t,p));CHECK(t.anchor.left_x==12000);
    p.left_x=12600;p.right_trigger=100;CHECK(observe(t,p));CHECK(t.anchor.left_x==12000);
    p.left_x=12900;CHECK(!observe(t,p));p.left_x=13200;CHECK(observe(t,p));
    passed("other controls cannot erase retained stick anchor");
}
static void route_handle_baselines() {
    SemanticPadTracker t{};GamepadState p{};CHECK(!observe(t,p));p.buttons=0x1000;p.left_x=20000;
    CHECK(!observe_semantic_pad(t,true,99,p));CHECK(!observe_semantic_pad(t,false,99,p));
    p={};CHECK(!observe_semantic_pad(t,true,99,p));CHECK(!observe_semantic_pad(t,true,0,p));
    p.buttons=0x1000;CHECK(!observe_semantic_pad(t,true,99,p));p.buttons=0;CHECK(observe_semantic_pad(t,true,99,p));
    passed("route loss/new handle initializes baseline without activity");
}
static void triggers() {
    SemanticPadTracker t{};GamepadState p{};CHECK(!observe(t,p));p.left_trigger=30;CHECK(!observe(t,p));
    p.left_trigger=64;CHECK(observe(t,p));p.left_trigger=67;CHECK(!observe(t,p));
    p.left_trigger=68;CHECK(observe(t,p));p.left_trigger=0;CHECK(observe(t,p));
    passed("trigger deadzone and retained delta thresholds");
}
static void radial_and_extremes() {
    SemanticPadTracker t{};GamepadState p{};CHECK(!observe(t,p));
    p.left_x=p.left_y=7000;CHECK(observe(t,p));
    p.left_x=p.left_y=-32768;p.right_x=p.right_y=-32768;CHECK(observe(t,p));
    p.left_x=p.left_y=32767;p.right_x=p.right_y=32767;CHECK(observe(t,p));
    passed("radial movement and opposite int16 extremes without overflow");
}
static void analog_noise_and_metadata() {
    SemanticAnalogTracker t{};AnalogData a{6,0.5f,0.0f,true};CHECK(!observe_semantic_analog(t,42,a));
    const float original=a.x;a.x=std::nextafter(a.x,1.0f);CHECK(a.x!=original);
    CHECK(!observe_semantic_analog(t,42,a));CHECK(t.anchor.x==original);
    a.active=false;a.x=0;CHECK(!observe_semantic_analog(t,42,a));
    a.active=true;a.x=0.8f;CHECK(!observe_semantic_analog(t,42,a));
    a.mode=8;a.x=-0.8f;CHECK(!observe_semantic_analog(t,42,a));
    a.x=0.8f;CHECK(!observe_semantic_analog(t,77,a));
    a.x=0;CHECK(observe_semantic_analog(t,77,a));
    passed("analog 1-ULP noise and active/mode/handle changes are not input");
}
static void analog_sticks() {
    for(int mode:{6,8}){SemanticAnalogTracker t{};AnalogData a{mode,0,0,true};CHECK(!observe_semantic_analog(t,42,a));
        for(float x:{0.01f,0.1f,0.2f}){a.x=x;CHECK(!observe_semantic_analog(t,42,a));}
        a.x=0.5f;CHECK(observe_semantic_analog(t,42,a));a.x=0.51f;CHECK(!observe_semantic_analog(t,42,a));
        a.x=0.52f;CHECK(!observe_semantic_analog(t,42,a));a.x=0.54f;CHECK(observe_semantic_analog(t,42,a));
        a.x=0;CHECK(observe_semantic_analog(t,42,a));}
    passed("normalized analog modes preserve slow motion and significant release");
}
static void analog_invalid_and_trigger() {
    SemanticAnalogTracker t{};AnalogData a{6,0,0,true};CHECK(!observe_semantic_analog(t,42,a));
    for(float bad:{std::numeric_limits<float>::quiet_NaN(),std::numeric_limits<float>::infinity(),1.1f}){
        a.x=bad;CHECK(!observe_semantic_analog(t,42,a));}
    a.mode=7;a.x=100;CHECK(!observe_semantic_analog(t,42,a));
    a={10,0,0,true};CHECK(!observe_semantic_analog(t,42,a));a.x=30.0f/255.0f;CHECK(!observe_semantic_analog(t,42,a));
    a.x=0.2f;CHECK(observe_semantic_analog(t,42,a));a.x=0.21f;CHECK(!observe_semantic_analog(t,42,a));
    a.x=0.22f;CHECK(observe_semantic_analog(t,42,a));a.x=0;CHECK(observe_semantic_analog(t,42,a));
    a.y=0.1f;CHECK(!observe_semantic_analog(t,42,a));
    passed("nonfinite/out-of-range/unsupported analog ignored; trigger uses its own units");
}

struct RecordedSample {unsigned slot;bool routed;U64 handle;GamepadState pad;};
template<std::size_t N>
static void recorded_trace(const char *name,const RecordedSample (&samples)[N],unsigned expected_raw,unsigned expected_semantic) {
    SemanticPadTracker trackers[4]{};GamepadState previous[4]{};U64 handles[4]{};bool connected[4]{};
    unsigned raw=0,semantic=0,routed=0;
    for(const auto &row:samples){unsigned slot=row.slot;CHECK(slot<4);
        if(row.routed){++routed;if(connected[slot]&&handles[slot]==row.handle&&!same_pad(row.pad,previous[slot]))++raw;}
        if(observe_semantic_pad(trackers[slot],row.routed,row.handle,row.pad))++semantic;
        connected[slot]=row.routed;handles[slot]=row.handle;previous[slot]=row.pad;
    }
    CHECK(routed>0);CHECK(raw==expected_raw);CHECK(semantic==expected_semantic);++cases;
    std::printf("PASS recorded %s: raw=%u semantic=%u routed_samples=%u\n",name,raw,semantic,routed);
}
'''


def trace_cpp(name, path):
    proof = json.loads(path.read_text())
    snapshots = [item for item in proof.get("samples", []) if item.get("event") == "steam_input_snapshot"]
    if not snapshots:
        raise RuntimeError("Trace has no recorded snapshots: " + str(path))
    fields = ("buttons", "left_trigger", "right_trigger", "left_x", "left_y", "right_x", "right_y")
    rows = []
    for snapshot in snapshots:
        for sample in snapshot.get("xinput", []):
            routed = sample.get("routed") is True
            state = sample.get("state", {})
            if routed and (sample.get("result") != 0 or not state):
                raise RuntimeError("Inconsistent routed trace row")
            values = [int(state.get(field, 0)) for field in fields]
            for i, value in enumerate(values):
                low, high = (0, 65535) if i == 0 else ((0, 255) if i < 3 else (-32768, 32767))
                if not low <= value <= high:
                    raise RuntimeError("Invalid recorded pad field")
            rows.append("{%d,%s,%dULL,{%s}}" % (int(sample["slot"]), str(routed).lower(),
                        int(sample.get("steam_input_handle", 0)), ",".join(map(str, values))))
    return "static const RecordedSample trace_" + name + "[]={\n" + ",\n".join(rows) + "\n};\n", len(snapshots)


def main():
    source = SOURCE.read_text()
    extracted = extract(source)
    compiler = shutil.which("clang++")
    if not compiler:
        raise RuntimeError("Host clang++ is required; no production probe is built")
    OUT.mkdir(parents=True, exist_ok=True)
    trace_definitions = []
    trace_calls = []
    records = []
    for name, path, expected_raw, expected_semantic in TRACES:
        definition, count = trace_cpp(name, path)
        trace_definitions.append(definition)
        trace_calls.append(f'recorded_trace("{name}",trace_{name},{expected_raw},{expected_semantic});')
        records.append({"path": str(path.relative_to(ROOT)), "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                        "snapshots": count, "raw_changes": expected_raw, "semantic_changes": expected_semantic})
    entry = r'''int main(){neutral_rounding();drift_inside_deadzones();buttons();stick_press_release();
        slow_accumulation();independent_anchors();route_handle_baselines();triggers();radial_and_extremes();
        analog_noise_and_metadata();analog_sticks();analog_invalid_and_trigger();
    ''' + "\n".join(trace_calls) + r'''
        std::printf("OFFLINE ONLY: %u semantic activity cases passed; no Steam/Wine calls.\n",cases);return 0;}
    '''
    harness = OUT / "activity-extracted.cpp"
    binary = OUT / "activity-extracted"
    harness.write_text(PRELUDE + "\n#include <initializer_list>\n" + extracted + "\n" + TESTS + "\n".join(trace_definitions) + entry)
    command = [compiler, "-std=c++17", "-O1", "-g", "-Wall", "-Wextra", "-Werror",
               "-fsanitize=address,undefined", "-fno-omit-frame-pointer", str(harness), "-o", str(binary)]
    subprocess.run(command, check=True)
    result = subprocess.run([str(binary)], check=True, text=True, capture_output=True)
    print(result.stdout, end="")
    if SOURCE.read_text() != source:
        raise RuntimeError("Production helper changed during the test; rerun for current source")
    (OUT / "report.json").write_text(json.dumps({"schema_version": 1, "offline_only": True,
        "source_sha256": hashlib.sha256(source.encode()).hexdigest(),
        "extracted_sha256": hashlib.sha256(extracted.encode()).hexdigest(), "cases_passed": 14,
        "traces": records, "compiler_command": command, "stdout": result.stdout, "stderr": result.stderr}, indent=2) + "\n")


if __name__ == "__main__":
    main()
