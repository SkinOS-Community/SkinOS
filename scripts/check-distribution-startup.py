#!/usr/bin/env python3
"""Verify a packaged app against a fresh data directory, without an account.

Uses the system-only PATH and an unavailable developer directory. The bounded
runtime probe denies external networking and existing user files; no login or
game is requested and the user's running SkinOS is left alone.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', required=True, type=Path)
    parser.add_argument('--work', required=True, type=Path)
    args = parser.parse_args()
    app, work = args.app.absolute(), args.work.absolute()
    if work.exists() or work.is_symlink():
        raise RuntimeError('Use a new clean-room directory')
    work.mkdir(parents=True, mode=0o700)
    env = os.environ.copy()
    for key in list(env):
        if key.startswith(('SKINOS_', 'Steam', 'STEAM_', 'DYLD_')):
            env.pop(key)
    env.update(SKINOS_DATA_DIR=str(work/'data'), PATH='/usr/bin:/bin',
               DEVELOPER_DIR='/skinos-distribution-without-developer-tools')
    host, core = (app/'Contents/MacOS'/name for name in ['skinos-runtime-host','skinos-core'])
    first = subprocess.run([str(host),'--prepare-bundled-runtime'],env=env,
                           capture_output=True,text=True,check=True,timeout=900)
    (work/'bootstrap.json').write_text(first.stdout)
    pin = json.loads((app/'Contents/Resources/runtime-payload/manifest.json').read_text())
    version = work/'data/runtime-source'/pin['runtime_sha256']
    for name, entry in pin['files'].items():
        path=version/name
        if path.is_symlink() or not path.is_file() or path.stat().st_size!=entry['size']:
            raise RuntimeError('Runtime file type/size mismatch: '+name)
        if hashlib.sha256(path.read_bytes()).hexdigest()!=entry['sha256']:
            raise RuntimeError('Runtime hash mismatch: '+name)
    for name,target in pin['symlinks'].items():
        if not (version/name).is_symlink() or str((version/name).readlink())!=target:
            raise RuntimeError('Unexpected framework link: '+name)
    again=subprocess.run([str(host),'--prepare-bundled-runtime'],env=env,
                         capture_output=True,text=True,check=True,timeout=60)
    (work/'reuse.json').write_text(again.stdout)
    probe=subprocess.run([str(host),'probe','--state-root',str(work/'runtime'),
                          '--phase','global','--json'],env=env,capture_output=True,
                         text=True,check=True,timeout=60)
    runtime=json.loads(probe.stdout)
    (work/'runtime.json').write_text(json.dumps(runtime,indent=2)+'\n')
    if (runtime['exit_code']!=0 or runtime['timed_out']
            or not all(runtime[key] for key in ['sandbox_guard_passed','global_user_created','clean_release_completed'])
            or runtime['authenticated'] or runtime['game_launch_verified']):
        raise RuntimeError('Signed isolated runtime lifecycle failed')
    commands=[{'id':name,'command':name,'payload':{}} for name in ['status','auto_login','quit']]
    result=subprocess.run([str(core)],input=''.join(json.dumps(c)+'\n' for c in commands),
                          env=env,capture_output=True,text=True,check=True,timeout=30)
    events=[json.loads(line) for line in result.stdout.splitlines()]
    if any(e.get('event') in ['authenticated','library','qr_challenge'] for e in events):
        raise RuntimeError('Clean distribution unexpectedly contained an account/session')
    status=next(e['payload'] for e in events if e.get('id')=='status' and e.get('event')=='status')
    automatic=next(e['payload'] for e in events if e.get('id')=='auto_login' and e.get('event')=='status')
    if status['state']!='signed_out' or status.get('accounts')!=[] or status.get('account') is not None:
        raise RuntimeError('Unexpected saved account in clean startup')
    if automatic['state']!='signed_out' or automatic.get('auto_login_disabled') is not True:
        raise RuntimeError('Unexpected automatic account selection in clean startup')
    summary={'passed':True,'app':str(app),'fresh_data_directory':str(work/'data'),
             'generic_runtime_files_verified':len(pin['files']),
             'framework_links_verified':len(pin['symlinks']),
             'developer_tools_required':False,'runtime_load_and_release_verified':True,
             'saved_accounts':0,'login_requested':False,'game_started':False,
             'core_exit_code':result.returncode}
    (work/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    print(json.dumps(summary))


if __name__=='__main__':
    main()
