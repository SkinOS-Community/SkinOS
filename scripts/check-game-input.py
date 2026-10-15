#!/usr/bin/env python3
"""Normal launch/stop QA with a temporarily enabled per-game Steam Input setting.

Uses the same core commands as the UI; no diagnostic launch or credential read.
Restores the exact saved settings only if they were not changed in the meantime.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('input_qa',ROOT/'scripts/check-steam-input-wine.py')
qa = importlib.util.module_from_spec(spec);spec.loader.exec_module(qa)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--core',required=True,type=Path)
    parser.add_argument('--appid',type=int,default=629520)
    parser.add_argument('--software-fixture',action='store_true')
    parser.add_argument('--reconnect-fixture',action='store_true',help='Detach and reattach only the owned SDL fixture during a running game')
    parser.add_argument('--require-rumble-reconnect',action='store_true',help='Require real SDK retirement and a fresh feedback binding during the reconnect check')
    parser.add_argument('--rumble-fixture',action='store_true',help='Drive actual XInput vibration through the owned software fixture')
    parser.add_argument('--rumble-dwell',type=float,default=.55,help='Seconds to retain each nonzero fixture request without another XInput call')
    parser.add_argument('--product-rumble',action='store_true',help='Exercise ordinary feedback with more than 64 real XInput requests, without fixture QA mode')
    parser.add_argument('--observer-runner','--wine-runner',type=Path)
    parser.add_argument('--observer-prefix','--wine-prefix',type=Path)
    parser.add_argument('--hold',type=float,default=5)
    parser.add_argument('--output',required=True,type=Path)
    args=parser.parse_args()
    rumble_requested=args.rumble_fixture or args.product_rumble
    if not .5<=args.rumble_dwell<=5:
        parser.error('--rumble-dwell must be between 0.5 and 5 seconds')
    if args.require_rumble_reconnect and not args.reconnect_fixture:
        parser.error('--require-rumble-reconnect requires --reconnect-fixture')
    if args.product_rumble and (not args.software_fixture or args.rumble_fixture or args.reconnect_fixture):
        parser.error('--product-rumble requires --software-fixture and excludes other fixture sequences')
    if args.rumble_fixture and (not args.software_fixture or args.reconnect_fixture):
        parser.error('--rumble-fixture requires --software-fixture and excludes --reconnect-fixture')
    if args.reconnect_fixture and not args.software_fixture:
        parser.error('--reconnect-fixture requires --software-fixture')
    if (args.reconnect_fixture or rumble_requested) and not (args.observer_runner and args.observer_prefix):
        parser.error('Reconnect/rumble checks require the selected --wine-runner and --wine-prefix')
    if (args.observer_runner or args.observer_prefix) and not (args.reconnect_fixture or rumble_requested):
        parser.error('Observer paths are only used by reconnect/rumble checks')
    if args.reconnect_fixture:args.hold=max(args.hold,14)
    if rumble_requested:args.hold=max(args.hold,10)
    if args.rumble_fixture:args.hold=max(args.hold,2*args.rumble_dwell+4)
    if args.output.exists():raise RuntimeError('Refusing to overwrite a prior QA result')
    env=os.environ.copy()
    for name in list(env):
        if name.startswith('SKINOS_INPUT_') or name=='SKINOS_STEAM_INPUT_FIXTURE':env.pop(name)
    if args.software_fixture:env['SKINOS_STEAM_INPUT_FIXTURE']='neutral'
    if args.reconnect_fixture:env['SKINOS_INPUT_RECONNECT_TEST']='1'
    if args.rumble_fixture:env['SKINOS_INPUT_FEEDBACK_TEST']='1'
    qa.require_exclusive_core()
    runtime_root=Path.home()/'Library/Application Support/SkinOs/runtime'
    prior_runtime_names={path.name for path in runtime_root.glob('probe-*')}
    proc=subprocess.Popen([str(args.core)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,text=True,env=env)
    inbox=queue.Queue()
    def reader():
        for line in proc.stdout:
            try:inbox.put(json.loads(line))
            except json.JSONDecodeError:pass
        inbox.put(None)
    threading.Thread(target=reader,daemon=True).start()
    monitor=qa.OriginalSteamMonitor()
    evidence={'appid':args.appid,'normal_launch_command':True,'physical_device_verified':False,
        'software_fixture':args.software_fixture,'reconnect_fixture':args.reconnect_fixture,'running_seen':False,'exited_seen':False,
        'settings_restored':False,'passed':False,'input_status':[],'rumble_status':[],'observer_readbacks':[]}
    observer=None;rumble_driver=None;rumble_ready=None;rumble_done=False
    evidence['rumble_fixture']=args.rumble_fixture; evidence['product_rumble']=args.product_rumble; evidence['rumble_requests']=[]
    if args.rumble_fixture:evidence['expected_rumble_dwell_seconds']=args.rumble_dwell
    motor_sequence=([(257*(i+1),0) if i%2==0 else (0,353*(i+1)) for i in range(80)]+[(0,0)]
        if args.product_rumble else [(16384,0),(0,0),(0,24576),(0,0)])
    original=enabled=None;account=None;changed=False;requested=False;terminal=False
    stopped=False;running_at=None;stage='authenticate';failure=None
    def send(command,ident,**payload):
        proc.stdin.write(json.dumps({'id':ident,'command':command,'payload':payload})+'\n');proc.stdin.flush()
    def receive(timeout=1):
        try:item=inbox.get(timeout=timeout)
        except queue.Empty:return {}
        if item is None:raise RuntimeError('Core event stream closed')
        return item
    def wait_for(ident,event,timeout=20):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            item=receive();payload=item.get('payload',{})
            if item.get('id')==ident and item.get('event')=='error':raise RuntimeError('Core rejected '+ident)
            if item.get('id')==ident and item.get('event')==event:return payload
        raise TimeoutError(ident)
    def interrupted(_signum,_frame):raise InterruptedError('Normal-game QA interrupted')
    signal.signal(signal.SIGTERM,interrupted);signal.signal(signal.SIGINT,interrupted)
    try:
        end=time.monotonic()+180
        while time.monotonic()<end:
            item=receive();kind=item.get('event');ident=item.get('id');payload=item.get('payload',{})
            if kind=='ready':send('auto_login','auth')
            elif kind=='authenticated':account=payload.get('steamid')
            elif kind=='library' and account and not payload.get('cached',True) and stage=='authenticate':
                if not any(g.get('appid')==args.appid and g.get('installed_dir') for g in payload.get('games',[])):
                    # Installed path field is intentionally not trusted here;
                    # the launch command performs its normal ownership/install checks.
                    if not any(g.get('appid')==args.appid for g in payload.get('games',[])):
                        raise RuntimeError('Target is absent from the owned library')
                send('get_game_settings','get-settings',appid=args.appid);stage='settings'
            elif kind=='game_settings' and ident=='get-settings':
                if payload.get('appid')!=args.appid:raise RuntimeError('Settings game identity mismatch')
                original=payload['settings'];enabled=dict(original,steam_input=True)
                send('save_game_settings','enable-input',appid=args.appid,settings=enabled);changed=True;stage='save'
            elif kind=='game_settings' and ident=='enable-input':
                if payload.get('settings',{}).get('steam_input') is not True:raise RuntimeError('Input setting was not saved')
                send('launch_game','play',appid=args.appid);requested=True;stage='play'
            elif kind=='launch_status' and ident=='play':
                if payload.get('appid')!=args.appid or payload.get('steamid') not in [None,account]:raise RuntimeError('Launch identity mismatch')
                state=payload.get('state');print('Game:',state,flush=True)
                if state=='running':
                    evidence['running_seen']=True;running_at=time.monotonic()
                    if args.reconnect_fixture and observer is None:
                        spec=importlib.util.spec_from_file_location('input_observer',ROOT/'scripts/input_reconnect_observer.py')
                        observer_module=importlib.util.module_from_spec(spec);spec.loader.exec_module(observer_module)
                        observer=observer_module.Observer(args.observer_runner,args.observer_prefix,args.output.with_suffix('').with_name(args.output.stem+'-observer'))
                        evidence['observer_readbacks'].append(observer.compare('before'))
                if state=='exited':evidence['exited_seen']=True
                if state=='failed':raise RuntimeError('Normal game launch failed; see its launch log')
            elif kind=='steam_input_status' and ident=='play':
                if payload.get('appid')!=args.appid or payload.get('steamid')!=account:raise RuntimeError('Input owner mismatch')
                evidence['input_status'].append({key:payload[key] for key in ['state','game_continues','controllers'] if key in payload})
                print('Input:',payload.get('state'),flush=True)
                if payload.get('state')=='failed':raise RuntimeError('Input degraded during normal-game QA')
                if observer is not None and payload.get('state') in ['waiting_reconnect','reconnected']:
                    evidence['observer_readbacks'].append(observer.compare('removed' if payload['state']=='waiting_reconnect' else 'after'))
            elif kind=='steam_input_rumble_status' and ident=='play':
                if payload.get('appid')!=args.appid or payload.get('steamid')!=account:raise RuntimeError('Rumble owner mismatch')
                evidence['rumble_status'].append({key:payload[key] for key in ['state','binding'] if key in payload})
                print('Rumble:',payload.get('state'),flush=True)
                if payload.get('state')=='failed':raise RuntimeError('Rumble feedback failed; see owned runtime proof')
                if payload.get('state')=='ready':rumble_ready=payload
            elif kind=='operation_complete' and ident=='play':terminal=True;break
            elif kind=='error' and ident in ['auth','get-settings','enable-input','play','stop']:
                if ident=='play':terminal=True
                raise RuntimeError('Core rejected '+str(ident)+'; see the application log')
            if rumble_requested and running_at is not None and rumble_ready is not None and not rumble_done:
                spec=importlib.util.spec_from_file_location('rumble_driver',ROOT/'scripts/game_rumble_driver.py')
                driver_module=importlib.util.module_from_spec(spec);spec.loader.exec_module(driver_module)
                rumble_driver=driver_module.Driver(args.observer_runner,args.observer_prefix,args.output.with_suffix('').with_name(args.output.stem+'-rumble'))
                device=rumble_driver.inspect();binding=rumble_ready.get('binding',{})
                if device.get('generation')!=binding.get('generation') or device.get('owned_slot')!=binding.get('slot'):
                    raise RuntimeError('External Windows device and parent binding differ')
                evidence['rumble_device']=device
                for left,right in motor_sequence:
                    result=rumble_driver.set_motors(left,right)
                    if result.get('xinput_set_called') is not True or result.get('xinput_result')!=0:
                        raise RuntimeError('External XInput call failed')
                    evidence['rumble_requests'].append({'left':left,'right':right,'result':result})
                    time.sleep(.02 if args.product_rumble else args.rumble_dwell if left or right else .55)
                evidence['rumble_driver_exit']=rumble_driver.close();rumble_driver=None
                if evidence['rumble_driver_exit']!=0:raise RuntimeError('External rumble driver did not exit cleanly')
                rumble_done=True
            if running_at is not None and not stopped and time.monotonic()-running_at>=args.hold:
                if rumble_requested and not rumble_done:
                    raise RuntimeError('SDK rumble binding was not ready during the running game')
                if observer is not None:
                    evidence['observer_exit']=observer.close();observer=None
                    if evidence['observer_exit']!=0:raise RuntimeError('Windows input observer did not exit cleanly')
                send('stop_game','stop',appid=args.appid);stopped=True
        if not terminal:raise TimeoutError('Normal launch did not drain')
        if not evidence['running_seen'] or not evidence['exited_seen']:raise RuntimeError('Missing real running/exit states')
        if args.reconnect_fixture:
            states=[item.get('state') for item in evidence['input_status']]
            if 'disconnected' not in states or 'reconnected' not in states or states.index('disconnected')>=states.index('reconnected'):
                raise RuntimeError('Missing ordered disconnect/reconnect input lifecycle during the same game launch')
            if len(evidence['observer_readbacks'])!=3 or evidence.get('observer_exit')!=0:
                raise RuntimeError('Missing actual Windows input readbacks around reconnect')
            if args.require_rumble_reconnect:
                rows=evidence['rumble_status']
                ready=next((i for i,row in enumerate(rows) if row.get('state')=='ready'),None)
                retired=next((i for i,row in enumerate(rows) if row.get('state')=='retired'),None)
                rebound=next((i for i,row in enumerate(rows) if row.get('state')=='reconnected'),None)
                if ready is None or retired is None or rebound is None or not ready<retired<rebound:
                    raise RuntimeError('Missing ordered real SDK Bind, Retire and rebind acknowledgements')
                old=rows[ready].get('binding',{}); new=rows[rebound].get('binding',{})
                if rows[retired].get('binding')!=old or not old or not new:
                    raise RuntimeError('SDK retirement did not match its previous feedback binding')
                if any(old.get(key)!=new.get(key) for key in ['epoch','control_session','slot']) or old.get('slot')!=0:
                    raise RuntimeError('Feedback reconnect changed the owned connection or slot')
                if not isinstance(old.get('generation'),int) or not isinstance(new.get('generation'),int) or new['generation']<=old['generation']:
                    raise RuntimeError('Feedback reconnected without a fresh driver generation')
                for phase,binding in [('before',old),('after',new)]:
                    observed=next(row.get('owned_identity') for row in evidence['observer_readbacks'] if row['phase']==phase)
                    if not observed or any(observed.get(key)!=binding.get(key) for key in ['slot','generation']):
                        raise RuntimeError('SDK feedback binding differs from the independently observed Windows device')
                evidence['rumble_reconnect_verified']=True
        if rumble_requested or args.require_rumble_reconnect:
            if rumble_requested and (not rumble_done or len(evidence['rumble_requests'])!=len(motor_sequence)):raise RuntimeError('Rumble requests were incomplete')
            matches=[]
            for path in runtime_root.glob('probe-*'):
                if path.name in prior_runtime_names:continue
                try:
                    meta=json.loads((path/'launch-meta.json').read_text())
                    if meta.get('appid')!=args.appid:continue
                    report=json.loads((path/'report.json').read_text())
                except (OSError,ValueError):continue
                proofs=[entry['detail'] for entry in report.get('events',[]) if entry.get('event')=='steam_input_rumble_feedback_proof']
                if len(proofs)==1:matches.append((path.name,report,proofs[0]))
            if len(matches)!=1:raise RuntimeError('No unique completed runtime rumble proof for this launch')
            name,report,proof=matches[0]
            evidence['rumble_runtime']={'runtime':name,'proof':proof,'exit_code':report.get('exit_code'),'clean_release_completed':report.get('clean_release_completed')}
            if report.get('exit_code')!=0 or not report.get('clean_release_completed'):raise RuntimeError('Rumble runtime cleanup unconfirmed')
            if args.product_rumble or args.require_rumble_reconnect:
                sink=proof.get('sink') or {}
                if proof.get('mode')!='product' or proof.get('session_ok') is not True:
                    raise RuntimeError('Ordinary feedback session did not complete cleanly')
                if args.product_rumble and (sink.get('sdk_dispatches',0)<len(motor_sequence) or (sink.get('last_driver_sequence') or 0)<len(motor_sequence)):
                    raise RuntimeError('Ordinary feedback stopped before all real XInput requests reached the SDK')
                if args.require_rumble_reconnect and (sink.get('bound')!=new or sink.get('last_retired_binding')!=old):
                    raise RuntimeError('Final SDK report lost the verified retirement and new binding')
            elif not all(proof.get(key) is True for key in ['software_feedback_verified','ordered_callback_verified','cleanup_seen']):
                raise RuntimeError('Full XInput to original SDL rumble feedback proof failed')
            if args.rumble_fixture:
                samples=(proof.get('callback_readback') or {}).get('callback_samples',[])
                held=[]
                for key in ['low','high']:
                    first=next((i for i,row in enumerate(samples) if row.get(key,0)>0),None)
                    zero=next((row for row in samples[(first or 0)+1:] if row.get('low')==0 and row.get('high')==0),None)
                    if first is None or zero is None:raise RuntimeError('Missing observed motor hold boundaries')
                    duration=(zero['elapsed_us']-samples[first]['elapsed_us'])/1_000_000
                    if duration<args.rumble_dwell-.15:raise RuntimeError('Original SDL motor state expired before the retained XInput request ended')
                    held.append(duration)
                evidence['observed_rumble_dwell_seconds']=held
            if proof.get('physical_device_verified') is not False:raise RuntimeError('Software fixture proof incorrectly claims physical verification')
        evidence['passed']=True
    except Exception as error:
        failure=str(error);print('FAILED:',failure,flush=True)
    finally:
        if rumble_driver is not None:
            try:evidence['rumble_driver_exit']=rumble_driver.close()
            except Exception:evidence['rumble_driver_cleanup_failed']=True
        if observer is not None:
            try:evidence['observer_exit']=observer.close()
            except Exception:evidence['observer_cleanup_failed']=True
        if requested and not terminal and proc.poll() is None:
            try:
                send('stop_game','cleanup-stop',appid=args.appid)
                until=time.monotonic()+35
                while time.monotonic()<until:
                    item=receive()
                    if item.get('id')=='play' and item.get('event') in ['operation_complete','error']:
                        terminal=True;break
            except (OSError,RuntimeError):pass
        if changed and proc.poll() is None:
            try:
                send('get_game_settings','settings-before-restore',appid=args.appid)
                current=wait_for('settings-before-restore','game_settings')['settings']
                if current==enabled:
                    send('save_game_settings','restore-settings',appid=args.appid,settings=original)
                    restored=wait_for('restore-settings','game_settings')
                    evidence['settings_restored']=restored.get('settings')==original
                else:evidence['settings_restore_skipped']='Settings changed after this test enabled input'
            except Exception:evidence['settings_restore_error']=True
        if proc.poll() is None:
            try:send('quit','quit');proc.wait(timeout=15)
            except (OSError,subprocess.TimeoutExpired):proc.terminate();proc.wait(timeout=5)
        evidence.update(monitor.close())
        evidence.update(core_exit_code=proc.returncode,game_drained=terminal,error=failure)
        evidence['passed'] &= terminal and evidence['settings_restored'] and proc.returncode==0 and not evidence['original_gui_seen'] and evidence['original_gui_observation_complete']
        args.output.parent.mkdir(parents=True,exist_ok=True)
        args.output.write_text(json.dumps(evidence,indent=2)+'\n')
    print(json.dumps({key:evidence.get(key) for key in ['appid','passed','error','core_exit_code','game_drained','settings_restored','original_gui_seen','original_gui_observation_complete']},ensure_ascii=False))
    if not evidence['passed']:raise SystemExit(1)


if __name__=='__main__':main()
