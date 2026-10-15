#!/usr/bin/env python3
"""Compile separate DirectInput-state experiment; never overwrite live artifacts or run Wine."""
import hashlib
import argparse
import importlib.util
import json
import pathlib
import re
import subprocess

ROOT=pathlib.Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('wine_input_builder',ROOT/'scripts/build-wine-input.py')
builder=importlib.util.module_from_spec(spec);spec.loader.exec_module(builder)
sources=builder.SOURCES
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--observer',action='store_true',help='Build separate read-only reconnect observer, preserving strict consumer artifacts')
args=parser.parse_args()
stem='consumer-observer' if args.observer else 'consumer-dinput'
output=ROOT/('target/wine-input-observer' if args.observer else 'target/wine-input-di-experiment');output.mkdir(parents=True,exist_ok=True)
include=builder.headers()
test=output/'dinput-expect-test'
builder.run(['clang','-std=c11','-Wall','-Wextra','-Werror','-O2',sources/'dinput-expect-test.c','-o',test]);builder.run([test])
files={}
wine_root=pathlib.Path('/Applications/CrossOver Preview.app/Contents/SharedSupport/CrossOver/lib/wine')
for arch,target,dll_arch in [('x64','x86_64-pc-windows-msvc','x86_64-windows'),('arm64','aarch64-pc-windows-msvc','aarch64-windows'),('x86','i686-pc-windows-msvc','i386-windows')]:
    folder=output/arch;folder.mkdir(exist_ok=True)
    libraries=[builder.import_library(folder,wine_root/dll_arch,arch,x) for x in ['kernel32.dll','ucrtbase.dll']]
    obj=folder/(stem+'.obj');program=folder/(stem+'.exe')
    builder.run([builder.LLVM/'clang','-target',target,'-std=c11','-O2','-Wall','-Wextra','-Werror','-ffreestanding','-fno-builtin','-fno-stack-protector',
                 '-isystem',include/'msvcrt','-isystem',include,'-I',sources,'-c',sources/(stem+'.c'),'-o',obj])
    aliases=[]
    if arch=='x86':
        symbols=subprocess.check_output([str(builder.LLVM/'llvm-nm'),'--undefined-only',str(obj)],text=True)
        for name,n in re.findall(r'__imp__([A-Za-z0-9_]+)@(\d+)',symbols):aliases.append('/alternatename:__imp__'+name+'@'+n+'=__imp__'+name)
    builder.run(builder.LINK+['/machine:'+arch,'/entry:mainCRTStartup','/subsystem:console','/nodefaultlib','/timestamp:0','/out:'+str(program)]+aliases+[obj]+libraries)
    files[str(program.relative_to(output))]=hashlib.sha256(program.read_bytes()).hexdigest()
(output/'manifest.json').write_text(json.dumps({'schema_version':1,'experiment':'reconnect_observer' if args.observer else 'directinput_state','runtime_tested':False,'wine_headers_commit':builder.PIN,'files':files},indent=2)+'\n')
print(json.dumps({'output':str(output),'files':files},indent=2))
