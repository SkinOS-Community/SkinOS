#!/usr/bin/env python3
"""Build only the isolated Wine haptics experiment. Never install/run Wine."""
import argparse
import hashlib
import importlib.util
import json
import pathlib
import sys

ROOT=pathlib.Path(__file__).resolve().parents[1]
SOURCE=ROOT/'third_party/notproton/integration/wine-input/rumble-experiment'
OUT=ROOT/'target/wine-input-rumble-experiment'

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--wine-root',type=pathlib.Path,default=pathlib.Path('/Applications/CrossOver Preview.app/Contents/SharedSupport/CrossOver'))
    args=parser.parse_args()
    spec=importlib.util.spec_from_file_location('wine_builder',ROOT/'scripts/build-wine-input.py')
    builder=importlib.util.module_from_spec(spec);spec.loader.exec_module(builder)
    builder.SOURCES=SOURCE
    sys.argv=['build-wine-input.py','--output-dir',str(OUT),'--wine-root',str(args.wine_root)]
    builder.main()
    manifest=json.loads((OUT/'manifest.json').read_text())
    include=builder.headers()
    for arch,target in [('x64','x86_64-pc-windows-msvc'),('arm64','aarch64-pc-windows-msvc'),('x86','i686-pc-windows-msvc')]:
        folder=OUT/arch
        wine=args.wine_root/'lib/wine'/{'x64':'x86_64-windows','arm64':'aarch64-windows','x86':'i386-windows'}[arch]
        hid=builder.import_library(folder,wine,arch,'hid.dll')
        obj=folder/'rumble-consumer.obj';exe=folder/'rumble-consumer.exe'
        builder.run([builder.LLVM/'clang','-target',target,'-std=c11','-O2','-Wall','-Wextra','-Werror',
                     '-ffreestanding','-fno-builtin','-fno-stack-protector','-isystem',include/'msvcrt',
                     '-isystem',include,'-I',SOURCE,'-c',SOURCE/'rumble-consumer.c','-o',obj])
        aliases=[]
        if arch=='x86':
            import re,subprocess
            symbols=subprocess.check_output([str(builder.LLVM/'llvm-nm'),'--undefined-only',str(obj)],text=True)
            aliases=['/alternatename:__imp__'+n+'@'+size+'=__imp__'+n for n,size in re.findall(r'__imp__([A-Za-z0-9_]+)@(\d+)',symbols)]
            # Wine's public HID headers use direct stdcall declarations rather
            # than dllimport; the generated import library exposes the normal
            # undecorated forwarding thunks for these real DLL exports.
            aliases+=['/alternatename:_'+n+'@'+size+'=_'+n for n,size in re.findall(r'\bU _(Hid[DP]_[A-Za-z0-9_]+)@(\d+)',symbols)]
        libs=[folder/(name+'.lib') for name in ['kernel32.dll','setupapi.dll','ucrtbase.dll']]+[hid]
        builder.run(builder.LINK+['/machine:'+arch,'/nodefaultlib','/timestamp:0','/subsystem:console',
                                  '/entry:mainCRTStartup','/out:'+str(exe)]+aliases+[obj]+libs)
        manifest['files'][str(exe.relative_to(OUT))]=hashlib.sha256(exe.read_bytes()).hexdigest()
    manifest.update(experiment='owned_wine_haptics_feedback',runtime_tested=False,native_sdk_dispatched=False,
                    feedback_bytes=48,input_report_id=0,input_hid_bytes=15,output_report_id=1,output_hid_bytes=5,feature_hid_bytes=17)
    manifest['sources']={str(p.relative_to(SOURCE)):hashlib.sha256(p.read_bytes()).hexdigest()
                         for p in SOURCE.iterdir() if p.is_file() and p.suffix in ['.h','.c']}
    (OUT/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    print(json.dumps({'output':str(OUT),'files':manifest['files']},indent=2))

if __name__=='__main__':main()
