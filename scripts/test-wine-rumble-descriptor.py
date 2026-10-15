#!/usr/bin/env python3
"""Compile/run the pinned Wine HID parser natively; never install or run Wine.

The parser and stock XInput waveform selector are unchanged source from the
checksum-verified cached SDK archive. Only debug output, pool allocation and
HidD_GetFeature's I/O boundary are substituted. No fake parser is used.
"""
import argparse
import hashlib
import json
import pathlib
import subprocess
import tarfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
PIN = '290276fb04e812b5c4c2338ba0b8b4bd7e4bef4c'
ARCHIVE_SHA = '9e7317b410c5363c5d0bf3ffff12d5a98dcba4a74a2900d11b7ef48c6bca0241'
SOURCE = ROOT / 'third_party/notproton/integration/wine-input/rumble-experiment'
DEBUG_SHIM = '''#ifndef SKINOS_HOST_WINE_DEBUG_H
#define SKINOS_HOST_WINE_DEBUG_H
#define WINE_DEFAULT_DEBUG_CHANNEL(x)
#define TRACE_ON(x) 0
#define TRACE(...) ((void)0)
#define ERR(...) ((void)0)
#define WARN(...) ((void)0)
#define FIXME(...) ((void)0)
#define wine_dbg_sprintf(...) "debug disabled"
#define wine_dbgstr_longlong(x) "debug disabled"
#define debugstr_w(x) "debug disabled"
#endif
'''


def sha(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output-dir', type=pathlib.Path,
                        default=ROOT / 'target/wine-rumble-descriptor-parser-test')
    args = parser.parse_args()
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    archive = ROOT / 'target/wine-input-sdk' / (PIN + '.tar.gz')
    if sha(archive.read_bytes()) != ARCHIVE_SHA:
        raise RuntimeError('Pinned Wine source archive checksum mismatch')
    source_hashes = {}
    with tarfile.open(archive) as tar:
        prefix = 'wine-' + PIN + '/'
        for member in tar:
            if not member.isfile() or not member.name.startswith(prefix + 'include/'):
                continue
            relative = pathlib.PurePosixPath(member.name[len(prefix):])
            if '..' in relative.parts or relative.is_absolute():
                raise RuntimeError('Unsafe source archive path')
            dest = output / relative
            data = tar.extractfile(member).read()
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(data)
        for name, dest in [('dlls/hidparse.sys/main.c', 'wine-main.c'),
                           ('dlls/hid/hidp.c', 'wine-hidp.c')]:
            data = tar.extractfile(prefix + name).read()
            source_hashes[name] = sha(data)
            (output / dest).write_bytes(data)
        data = tar.extractfile(prefix + 'dlls/xinput1_3/main.c').read()
        source_hashes['dlls/xinput1_3/main.c'] = sha(data)
        text = data.decode()
        begin = text.index('static void check_waveform_caps(')
        end = text.index('\n}\n', begin) + 3
        (output / 'wine-xinput-waveform.c').write_text(text[begin:end])
        source_hashes['extracted check_waveform_caps'] = sha(text[begin:end].encode())
    shim = output / 'shim/wine'
    shim.mkdir(parents=True, exist_ok=True)
    (shim / 'debug.h').write_text(DEBUG_SHIM)
    common = ['clang', '-std=c11', '-D__WINESRC__', '-fshort-wchar', '-fms-extensions',
              '-Wno-ignored-attributes', '-Wno-pragma-pack', '-include', 'string.h',
              '-include', 'stdint.h', '-I' + str(output / 'shim'),
              '-isystem', str(output / 'include'), '-I' + str(output),
              '-O1', '-g', '-fsanitize=address,undefined', '-fno-sanitize=alignment',
              '-fno-sanitize-recover=all']
    # The upstream parser deliberately reads unaligned short HID items. Leave
    # alignment instrumentation off; ASan and all other UBSan checks remain on.
    objects = []
    for source in [output / 'wine-main.c', output / 'wine-hidp.c', SOURCE / 'parser-host.c']:
        obj = output / (source.stem + '.o')
        subprocess.run(common + ['-c', str(source), '-o', str(obj)], check=True)
        objects.append(str(obj))
    binary = output / 'parser-host'
    subprocess.run(['clang', '-fsanitize=address,undefined', '-Wl,-dead_strip',
                    *objects, '-o', str(binary)], check=True)
    result = subprocess.run([str(binary)], capture_output=True, text=True)
    (output / 'stdout.txt').write_text(result.stdout)
    (output / 'stderr.txt').write_text(result.stderr)
    own_hashes = {str(p.relative_to(ROOT)): sha(p.read_bytes()) for p in [
        SOURCE / 'parser-host.c', SOURCE / 'adapter.h', SOURCE / 'haptics.h', SOURCE.parent / 'adapter.h']}
    report = {'schema_version': 1, 'wine_commit': PIN, 'wine_archive_sha256': ARCHIVE_SHA,
              'source_sha256': source_hashes, 'owned_source_sha256': own_hashes,
              'parser_sources_modified': False, 'wine_executed': False,
              'physical_motor_verified': False, 'hid_feature_io': 'owned static fixture',
              'sanitizers': 'address,undefined except intentional upstream unaligned item reads',
              'exit_code': result.returncode, 'passed': result.returncode == 0,
              'stdout': result.stdout, 'stderr': result.stderr}
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    print(result.stdout, end='')
    if result.stderr:
        print(result.stderr, end='')
    if result.returncode:
        raise SystemExit(result.returncode)
    print('Report:', output / 'report.json')


if __name__ == '__main__':
    main()
