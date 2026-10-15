#!/usr/bin/env python3
"""Build owned Wine input transport PE files; never install or launch Wine."""
import argparse
import hashlib
import io
import json
import pathlib
import re
import subprocess
import tarfile
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
PIN = '290276fb04e812b5c4c2338ba0b8b4bd7e4bef4c'
ARCHIVE_SHA = '9e7317b410c5363c5d0bf3ffff12d5a98dcba4a74a2900d11b7ef48c6bca0241'
SOURCES = ROOT / 'third_party/notproton/integration/wine-input'
LLVM = pathlib.Path('/opt/homebrew/opt/llvm/bin')
RUSTROOT = pathlib.Path(subprocess.check_output(['rustc','--print','sysroot'],text=True).strip())
HOST = next(x.split(': ',1)[1] for x in subprocess.check_output(['rustc','-vV'],text=True).splitlines() if x.startswith('host: '))
LLD = RUSTROOT/'lib/rustlib'/HOST/'bin/rust-lld'
if not LLD.is_file():
    LLD = next((pathlib.Path.home()/'.rustup/toolchains').glob('*/lib/rustlib/'+HOST+'/bin/rust-lld'))
LINK = [LLD, '-flavor', 'link']

def run(args):
    subprocess.run([str(x) for x in args], check=True)

def headers():
    cache = ROOT / 'target/wine-input-sdk'
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / (PIN + '.tar.gz')
    if not archive.exists():
        data = urllib.request.urlopen('https://codeload.github.com/ValveSoftware/wine/tar.gz/' + PIN, timeout=60).read()
        if hashlib.sha256(data).hexdigest() != ARCHIVE_SHA:
            raise RuntimeError('Wine header archive checksum mismatch')
        archive.write_bytes(data)
    data = archive.read_bytes()
    if hashlib.sha256(data).hexdigest() != ARCHIVE_SHA:
        raise RuntimeError('Cached Wine header archive checksum mismatch')
    destination = cache / PIN
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as tar:
        for member in tar.getmembers():
            relative = pathlib.PurePosixPath(*pathlib.PurePosixPath(member.name).parts[1:])
            if '..' in relative.parts or relative.is_absolute() or not member.isfile():
                continue
            if relative.parts and relative.parts[0] == 'include':
                path = destination / relative
                content=tar.extractfile(member).read()
                if not path.exists() or path.read_bytes()!=content:
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(content)
    return destination / 'include'

def import_library(folder, wine, machine, name):
    path = wine / name
    output = subprocess.check_output([str(LLVM/'llvm-readobj'), '--coff-exports', str(path)], text=True)
    exports = re.findall(r'^  Name: ([^\r\n]+)$', output, re.M)
    if not exports:
        raise RuntimeError('No exported APIs in ' + str(path))
    definition = folder / (name + '.def')
    definition.write_text('LIBRARY ' + name + '\nEXPORTS\n' + '\n'.join(exports) + '\n')
    library = folder / (name + '.lib')
    run(LINK + ['/lib', '/machine:'+machine, '/def:'+str(definition), '/out:'+str(library)])
    return library

def inf():
    return '''[Version]
Signature="$Windows NT$"
Class=System
ClassGuid={4D36E97D-E325-11CE-BFC1-08002BE10318}
Provider=%Provider%
DriverVer=10/13/2026,1.0.0.0

[Manufacturer]
%Provider%=Devices,NTamd64,NTarm64
[Devices.NTamd64]
%Description%=Install,Root\\SkinOsInputBus
[Devices.NTarm64]
%Description%=Install,Root\\SkinOsInputBus
[Install.NT]
CopyFiles=DriverCopy
[DriverCopy]
skinosinput.sys
[DestinationDirs]
DriverCopy=12
[SourceDisksNames]
1="SkinOS Wine input transport",,,
[SourceDisksFiles]
skinosinput.sys=1
[Install.NT.Services]
AddService=SkinOsInputBus,0x00000002,DriverService
[DriverService]
DisplayName=%Description%
ServiceType=1
StartType=3
ErrorControl=1
ServiceBinary=%12%\\skinosinput.sys
[Strings]
Provider="SkinOS"
Description="SkinOS Wine input transport"
'''

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output-dir',type=pathlib.Path,default=ROOT/'target/wine-input')
    parser.add_argument('--wine-root',type=pathlib.Path,default=pathlib.Path('/Applications/CrossOver Preview.app/Contents/SharedSupport/CrossOver'))
    parser.add_argument('--arch',action='append',choices=['x64','arm64','x86'])
    args=parser.parse_args()
    include=headers()
    output=args.output_dir.resolve();output.mkdir(parents=True,exist_ok=True)
    host_test=output/'adapter-test'
    run(['clang','-std=c11','-Wall','-Wextra','-Werror','-O2',SOURCES/'adapter-test.c','-o',host_test])
    run([host_test])
    files={}
    for arch in args.arch or ['x64','arm64','x86']:
        target={'x64':'x86_64-pc-windows-msvc','arm64':'aarch64-pc-windows-msvc','x86':'i686-pc-windows-msvc'}[arch]
        wine=args.wine_root/'lib/wine'/{'x64':'x86_64-windows','arm64':'aarch64-windows','x86':'i386-windows'}[arch]
        if not wine.is_dir():raise RuntimeError('Missing build import source '+str(wine))
        folder=output/arch;folder.mkdir(exist_ok=True)
        libs={name:import_library(folder,wine,arch,name) for name in ['kernel32.dll','setupapi.dll','newdev.dll','advapi32.dll','shell32.dll','ntoskrnl.exe','hal.dll','ucrtbase.dll','cfgmgr32.dll']}
        common=[LLVM/'clang','-target',target,'-std=c11','-O2','-Wall','-Wextra','-Werror','-ffreestanding','-fno-builtin','-fno-stack-protector','-isystem',include/'msvcrt','-isystem',include,'-I',SOURCES]
        programs=[('consumer.c','consumer')] if arch=='x86' else [('feeder.c','feeder'),('consumer.c','consumer'),('driver.c','skinosinput')]
        for source,stem in programs:
            obj=folder/(stem+'.obj')
            run(common+['-c',SOURCES/source,'-o',obj])
            driver=stem=='skinosinput';dest=folder/(stem+('.sys' if driver else '.exe'))
            links=[libs[x] for x in (['ntoskrnl.exe','hal.dll'] if driver else ['kernel32.dll','setupapi.dll','newdev.dll','advapi32.dll','shell32.dll','ucrtbase.dll','cfgmgr32.dll'])]
            flags=['/driver','/subsystem:native','/entry:DriverEntry'] if driver else ['/subsystem:console','/entry:mainCRTStartup']
            aliases=[]
            if arch=='x86':
                symbols=subprocess.check_output([str(LLVM/'llvm-nm'),'--undefined-only',str(obj)],text=True)
                for name in re.findall(r'__imp__([A-Za-z0-9_]+)@(\d+)',symbols):
                    aliases.append('/alternatename:__imp__'+name[0]+'@'+name[1]+'=__imp__'+name[0])
            run(LINK+['/machine:'+arch,'/nodefaultlib','/timestamp:0','/out:'+str(dest)]+flags+aliases+[obj]+links)
            files[str(dest.relative_to(output))]=hashlib.sha256(dest.read_bytes()).hexdigest()
        if arch!='x86':
            (folder/'skinosinput.inf').write_text(inf())
            files[str((folder/'skinosinput.inf').relative_to(output))]=hashlib.sha256((folder/'skinosinput.inf').read_bytes()).hexdigest()
    (output/'manifest.json').write_text(json.dumps({'schema_version':1,'wine_headers_commit':PIN,'wine_archive_sha256':ARCHIVE_SHA,'files':files},indent=2)+'\n')
    print(json.dumps({'output':str(output),'files':files},indent=2))

if __name__=='__main__':main()
