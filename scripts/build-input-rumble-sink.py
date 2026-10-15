#!/usr/bin/env python3
"""Build isolated retained SDK sink probes; never mutate/run production or Wine."""
import difflib
import hashlib
import json
import pathlib
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'third_party/notproton/integration'
OUT = ROOT / 'target/input-rumble-sink-experiment'


def transform(source):
    replacements = [
        ('static ProbeContext context;\n', 'static ProbeContext context;\nstatic void input_rumble_sink_cleanup();\n'),
        ('static void cleanup() {\n', 'static void cleanup() {\n    input_rumble_sink_cleanup();\n'),
        ('#include "input-rumble-qa.h"\n', '#include "input-rumble-qa.h"\n#include "input-rumble-sink.h"\n'),
        ('    context.input=input;context.input_initialized=true;\n',
         '    context.input=input;context.input_initialized=true;\n    run_input_rumble_sink(utils,nullptr,expected_appid,decimal(expected));\n'),
    ]
    for before, after in replacements:
        if source.count(before) != 1:
            raise RuntimeError('Changed retained-sink integration boundary: ' + before)
        source = source.replace(before, after)
    return source


def main():
    folder = OUT / 'source/integration'
    folder.mkdir(parents=True, exist_ok=True)
    original = (SOURCE/'input-probe.cpp').read_text()
    candidate = transform(original)
    (folder/'input-probe.cpp').write_text(candidate)
    for name in ['sdk-client.cpp', 'input-rumble-qa.h', 'input-rumble-sink.h']:
        shutil.copyfile(SOURCE/name, folder/name)
    (OUT/'input-probe-integration.patch').write_text(''.join(difflib.unified_diff(
        original.splitlines(True), candidate.splitlines(True), fromfile='a/input-probe.cpp', tofile='b/input-probe.cpp')))
    # Reuse the established freestanding PE builder as an isolated copy. Only
    # this candidate gains imports needed by the retained input/output loop.
    builder = (ROOT/'scripts/build-input-probe.py').read_text()
    anchor = "'GetStdHandle':4, 'WriteFile':20, 'ExitProcess':4, 'Sleep':4, 'GetTickCount':0, 'GetLastError':0}"
    if builder.count(anchor) != 1: raise RuntimeError('Changed kernel import boundary')
    builder = builder.replace(anchor, anchor[:-1] + ", 'ReadFile':20, 'CreateThread':24, 'CloseHandle':4}")
    build_copy = OUT/'build-input-probe.py'
    build_copy.write_text(builder)
    subprocess.run(['python3', str(build_copy), '--source-root', str(OUT/'source'), '--output-dir', str(OUT/'pe')], check=True)
    paths = [OUT/'pe/input-probe.exe', OUT/'pe/input-probe32.exe', OUT/'pe/input-probe-manifest.json',
             OUT/'input-probe-integration.patch', folder/'input-probe.cpp', folder/'input-rumble-sink.h']
    manifest = {'schema_version':1, 'experiment':'retained_genuine_sdk_rumble_sink', 'executed':False,
                'original_probe_sha256':hashlib.sha256(original.encode()).hexdigest(),
                'files':{str(path.relative_to(OUT)):hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}}
    (OUT/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    print(json.dumps({'manifest':str(OUT/'manifest.json'),'executed':False}))


if __name__ == '__main__': main()
