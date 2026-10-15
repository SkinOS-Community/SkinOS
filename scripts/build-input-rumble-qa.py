#!/usr/bin/env python3
"""Build isolated opt-in rumble probes and integration diff; never run Wine."""
import difflib
import hashlib
import json
import pathlib
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'third_party/notproton/integration'
OUT = ROOT / 'target/input-rumble-qa'


def transform(source):
    if '#include "input-rumble-qa.h"' in source:
        for boundary in ['#include "input-rumble-qa.h"', '    input_rumble_qa_cleanup();',
                         '    run_input_rumble_qa(utils,xget,expected_appid,decimal(expected));']:
            if source.count(boundary) != 1:
                raise RuntimeError('Unexpected integrated rumble boundary: ' + boundary)
        return source
    replacements = [
        ('static ProbeContext context;\n', 'static ProbeContext context;\nstatic void input_rumble_qa_cleanup();\n'),
        ('static void cleanup() {\n', 'static void cleanup() {\n    input_rumble_qa_cleanup();\n'),
        ('extern "C" void mainCRTStartup(void) {\n', '#include "input-rumble-qa.h"\n\nextern "C" void mainCRTStartup(void) {\n'),
        ('    bool device_seen=controller_samples!=0,activity_verified=',
         '    run_input_rumble_qa(utils,xget,expected_appid,decimal(expected));\n    bool device_seen=controller_samples!=0,activity_verified='),
    ]
    for before, after in replacements:
        if source.count(before) != 1:
            raise RuntimeError('Expected one stable integration boundary: ' + before)
        source = source.replace(before, after)
    return source


def main():
    folder = OUT / 'source/integration'
    folder.mkdir(parents=True, exist_ok=True)
    original = (SOURCE / 'input-probe.cpp').read_text()
    candidate = transform(original)
    (folder / 'input-probe.cpp').write_text(candidate)
    for name in ['sdk-client.cpp', 'input-rumble-qa.h']:
        shutil.copyfile(SOURCE / name, folder / name)
    patch = ''.join(difflib.unified_diff(original.splitlines(True), candidate.splitlines(True),
        fromfile='a/third_party/notproton/integration/input-probe.cpp',
        tofile='b/third_party/notproton/integration/input-probe.cpp'))
    (OUT / 'input-probe-integration.patch').write_text(patch)
    subprocess.run(['python3', str(ROOT/'scripts/build-input-probe.py'), '--source-root', str(OUT/'source'),
                    '--output-dir', str(OUT/'pe')], check=True)
    files = {}
    for path in [OUT/'pe/input-probe.exe', OUT/'pe/input-probe32.exe', OUT/'input-probe-integration.patch',
                 folder/'input-probe.cpp', folder/'input-rumble-qa.h']:
        files[str(path.relative_to(OUT))] = hashlib.sha256(path.read_bytes()).hexdigest()
    manifest = {'schema_version': 1, 'experiment': 'genuine_sdk_rumble_sink', 'executed': False,
                'original_probe_sha256': hashlib.sha256(original.encode()).hexdigest(), 'files': files}
    (OUT/'manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
    print(json.dumps(manifest, indent=2))


if __name__ == '__main__':
    main()
