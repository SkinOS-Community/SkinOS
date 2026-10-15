#!/usr/bin/env python3
"""Check download-region settings through the real core in a fresh data directory.

No login, account import, user settings, or game transfer is requested.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--core', type=Path, required=True)
    parser.add_argument('--work', type=Path, required=True)
    args = parser.parse_args()
    work = args.work.absolute()
    if work.exists():
        raise RuntimeError('Use a new test directory')
    work.mkdir(parents=True)
    data = work / 'data'
    data.mkdir()
    original = {'steamapps_dir': str(work / 'steamapps'), 'crossover_path': None}
    settings_path = data / 'settings.json'
    settings_path.write_text(json.dumps(original))
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(('SKINOS_', 'STEAM_', 'Steam', 'DYLD_'))}
    env['SKINOS_DATA_DIR'] = str(data)

    def core(*commands):
        requests = [{'id': str(i), 'command': name, 'payload': payload}
                    for i, (name, payload) in enumerate(commands)]
        requests.append({'id': 'quit', 'command': 'quit', 'payload': {}})
        run = subprocess.run([str(args.core.absolute())], env=env,
                             input=''.join(json.dumps(item) + '\n' for item in requests),
                             text=True, capture_output=True, check=True, timeout=30)
        events = [json.loads(line) for line in run.stdout.splitlines()]
        if any(item.get('event') in ('authenticated', 'library', 'qr_challenge') for item in events):
            raise RuntimeError('Settings test unexpectedly used an account')
        return {event['id']: event for event in events if event.get('id') is not None}

    initial = core(('get_settings', {}))['0']['payload']
    assert initial['settings']['download_region_id'] is None, 'Old settings must migrate to automatic'
    regions = initial['download_regions']
    ids = [region['id'] for region in regions]
    assert len(ids) > 100 and len(set(ids)) == len(ids) and all(i > 0 for i in ids)
    assert all(region['name'].strip() for region in regions)
    selected = next(region for region in regions if region['id'] == 5)
    desired = dict(original, download_region_id=selected['id'])
    saved = core(('save_settings', {'settings': desired}), ('get_settings', {}))
    assert saved['0']['event'] == 'settings'
    assert saved['1']['payload']['settings'] == desired
    restarted = core(('get_settings', {}))['0']['payload']['settings']
    assert restarted == desired, 'Region must survive core restart'
    before = settings_path.read_bytes()
    for invalid in (0, 4294967295):
        rejected = core(('save_settings', {'settings': dict(original, download_region_id=invalid)}))['0']
        assert rejected['event'] == 'error', 'Unknown region must be rejected'
        assert settings_path.read_bytes() == before, 'Rejected save must not change preferences'
    reset = core(('save_settings', {'settings': dict(original, download_region_id=None)}))['0']
    assert reset['event'] == 'settings' and reset['payload']['settings']['download_region_id'] is None
    report = {'passed': True, 'catalog_count': len(regions), 'selected_region': selected,
              'old_settings_migration': True, 'restart_persistence': True,
              'invalid_ids_preserve_file': True, 'automatic_reset': True,
              'login_requested': False, 'game_started': False, 'download_requested': False}
    (work / 'summary.json').write_text(json.dumps(report, indent=2, ensure_ascii=False) + '\n')
    print(json.dumps(report, ensure_ascii=False))


if __name__ == '__main__':
    main()
