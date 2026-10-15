#!/usr/bin/env python3
"""Build the opt-in real Steam Input diagnostic without touching normal SDK probes."""
import argparse
import hashlib
import json
import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[1]

def build_loader_fixture(destination, sources, lld, imports):
    """Build neutral DLL search regression fixtures; never launch Wine or Steam."""
    destination.mkdir(parents=True, exist_ok=True)
    for arch, target in [('x64', 'x86_64-pc-windows-msvc'), ('x86', 'i686-pc-windows-msvc')]:
        folder = destination / arch
        libraries = folder / 'separate-dll-directory'
        programs = folder / 'probe-directory'
        libraries.mkdir(parents=True, exist_ok=True)
        programs.mkdir(exist_ok=True)
        (folder / 'sibling.c').write_text('__declspec(dllexport) int fixture_dependency(void) { return 42; }\n')
        (folder / 'client.c').write_text('__declspec(dllimport) int fixture_dependency(void);\n__declspec(dllexport) int fixture_answer(void) { return fixture_dependency(); }\n')
        for name in ('sibling', 'client'):
            subprocess.run(['clang', '-target', target, '-c', '-O2', '-ffreestanding',
                            '-fno-stack-protector', '-o', str(folder / (name + '.obj')),
                            str(folder / (name + '.c'))], check=True)
            args = [str(lld), '-flavor', 'link', '/machine:' + arch, '/dll', '/noentry', '/nodefaultlib',
                    '/out:' + str(libraries / (name + '.dll')), '/implib:' + str(folder / (name + '.lib')),
                    str(folder / (name + '.obj'))]
            if name == 'client': args.append(str(folder / 'sibling.lib'))
            subprocess.run(args, check=True)
        fixture = folder / 'loader-fixture.cpp'
        fixture.write_text('#define mainCRTStartup skinos_unused_main\n#include ' + json.dumps(str(sources / 'input-probe.cpp')) + '''
#undef mainCRTStartup
extern "C" void mainCRTStartup(void) {
    char path[2048];
    if(!env("SKINOS_LOADER_FIXTURE_DLL",path,sizeof(path)))fail("missing_fixture_path");
    if(LoadLibraryA(path))fail("baseline_unexpectedly_resolved_dependency");
    Handle module=load_steamclient(path);
    auto answer=reinterpret_cast<int (*)(void)>(GetProcAddress(module,"fixture_answer"));
    if(!answer||answer()!=42)fail("fixture_dependency_execution_failed");
    out("{\\"event\\":\\"loader_fixture\\",\\"baseline_missing_dependency\\":true,\\"altered_search_resolved\\":true}\\n");
    ExitProcess(0);
}
''')
        definition = folder / 'kernel32.def'
        definition.write_text('LIBRARY KERNEL32.dll\nEXPORTS\n' + '\n'.join(imports) + '\n')
        library = folder / 'kernel32.lib'
        subprocess.run([str(lld), '-flavor', 'link', '/lib', '/machine:' + arch,
                        '/def:' + str(definition), '/out:' + str(library)], check=True)
        objects = []
        for src in [fixture, sources / 'sdk-client.cpp']:
            obj = folder / (src.stem + '.obj')
            subprocess.run(['clang', '-target', target, '-c', '-O2', '-Wall', '-Wextra', '-Werror',
                '-std=c++11', '-ffreestanding', '-fno-exceptions', '-fno-rtti', '-fno-stack-protector',
                '-o', str(obj), str(src)], check=True)
            objects.append(str(obj))
        aliases = ['/alternatename:__imp__%s@%d=__imp__%s' % (name,size,name) for name,size in imports.items()] if arch=='x86' else []
        program = programs / 'loader-fixture.exe'
        subprocess.run([str(lld), '-flavor', 'link', '/machine:' + arch, '/entry:mainCRTStartup',
                        '/subsystem:console', '/nodefaultlib', '/out:' + str(program)] + aliases + objects + [str(library)], check=True)
        print(json.dumps({'architecture':arch, 'program':str(program), 'dll':str(libraries / 'client.dll')}))

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output-dir', type=pathlib.Path, default=ROOT / 'target/notproton')
    parser.add_argument('--source-root', type=pathlib.Path, default=ROOT / 'third_party/notproton')
    parser.add_argument('--loader-fixture-dir', type=pathlib.Path,
        help='Build neutral DLL-loader regression fixtures here instead of production probes; does not run them')
    args = parser.parse_args()
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    sysroot = pathlib.Path(subprocess.check_output(['rustc', '--print', 'sysroot'], text=True).strip())
    host = next(line.split(': ', 1)[1] for line in subprocess.check_output(['rustc', '-vV'], text=True).splitlines() if line.startswith('host: '))
    lld = sysroot / 'lib/rustlib' / host / 'bin/rust-lld'
    if not lld.is_file():
        candidates = list((pathlib.Path.home() / '.rustup/toolchains').glob('*/lib/rustlib/' + host + '/bin/rust-lld'))
        if not candidates: raise RuntimeError('A host rust-lld is required')
        lld = candidates[0]
    imports = {'LoadLibraryA':4, 'LoadLibraryExA':12, 'GetProcAddress':8, 'GetEnvironmentVariableA':12,
               'GetStdHandle':4, 'WriteFile':20, 'ExitProcess':4, 'Sleep':4, 'GetTickCount':0, 'GetLastError':0}
    sources = args.source_root.resolve() / 'integration'
    if args.loader_fixture_dir:
        build_loader_fixture(args.loader_fixture_dir.resolve(), sources, lld, imports)
        return
    definition = output / 'input-kernel32.def'
    definition.write_text('LIBRARY KERNEL32.dll\nEXPORTS\n' + '\n'.join(imports) + '\n')
    for arch, target, stem in [('x64','x86_64-pc-windows-msvc','input-probe'), ('x86','i686-pc-windows-msvc','input-probe32')]:
        library = output / ('input-kernel32-' + arch + '.lib')
        subprocess.run([str(lld), '-flavor','link','/lib','/machine:'+arch,'/def:'+str(definition),'/out:'+str(library)],check=True)
        objects = []
        for name in ['input-probe.cpp','sdk-client.cpp']:
            obj = output / (stem + '-' + pathlib.Path(name).stem + '.obj')
            subprocess.run(['clang','-target',target,'-c','-O2','-Wall','-Wextra','-Werror','-std=c++11',
                '-fno-stack-protector','-ffreestanding','-fno-exceptions','-fno-rtti','-o',str(obj),str(sources/name)],check=True)
            objects.append(str(obj))
        aliases = ['/alternatename:__imp__%s@%d=__imp__%s' % (name,size,name) for name,size in imports.items()] if arch=='x86' else []
        subprocess.run([str(lld),'-flavor','link','/machine:'+arch,'/entry:mainCRTStartup','/subsystem:console',
            '/nodefaultlib','/out:'+str(output/(stem+'.exe'))]+aliases+objects+[str(library)],check=True)
        print(output/(stem+'.exe'))
    manifest = {'schema_version': 1, 'files': {
        name: hashlib.sha256((output/name).read_bytes()).hexdigest()
        for name in ['input-probe.exe', 'input-probe32.exe']}}
    (output/'input-probe-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')

if __name__ == '__main__': main()
