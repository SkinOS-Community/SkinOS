#!/usr/bin/env python3
"""Explicit fresh-prefix XInput -> actual owned HID feedback QA; never Steam SDK proof.

This script starts Wine only when invoked deliberately by the parent test runner.
Importing it is read-only. It refuses existing prefixes and nonempty log folders.
"""
import argparse
import hashlib
import importlib.util
import json
import os
import pathlib
import re
import shutil
import struct
import subprocess
import time

ROOT=pathlib.Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('input_helpers',ROOT/'scripts/check-wine-input.py')
base=importlib.util.module_from_spec(spec);spec.loader.exec_module(base)

MAX_FEEDBACK_BATCH=8

def decode_feedback(envelope,session,generation,last_sequence):
    """Validate an entire reply before exposing any record or advancing state.

    The fixed batch cap applies before flattening. Old single-record replies
    remain valid, but both formats must prove the same actual HID dequeue.
    This function has no side effects and never starts Wine.
    """
    def uint(value,key,maximum=0xffffffff):
        number=value.get(key)
        if type(number) is not int or not 0<=number<=maximum:
            raise RuntimeError('Malformed feedback '+key)
        return number
    if not isinstance(envelope,dict) or envelope.get('event')!='wine_input_feedback':
        raise RuntimeError('Malformed feedback envelope')
    if envelope.get('ok') is not True or uint(envelope,'win32_error')!=0:
        raise RuntimeError('Feedback read failed: '+str(envelope.get('win32_error')))
    if envelope.get('loss_latched',False) is not False:
        raise RuntimeError('Driver reported lost feedback')
    available=envelope.get('available')
    if type(available) is not bool:raise RuntimeError('Malformed feedback availability')
    if 'records' in envelope or 'batch_version' in envelope:
        rows=envelope.get('records')
        if (uint(envelope,'batch_version')!=1 or not isinstance(rows,list) or
                len(rows)>MAX_FEEDBACK_BATCH or available!=bool(rows) or
                envelope.get('drained') is not (len(rows)<MAX_FEEDBACK_BATCH)):
            raise RuntimeError('Malformed bounded feedback batch')
    else:
        rows=[envelope] if available else []
    checked=[];sequence=last_sequence
    for row in rows:
        if not isinstance(row,dict):raise RuntimeError('Malformed feedback record')
        next_sequence=uint(row,'sequence_low')|(uint(row,'sequence_high')<<32)
        if (uint(row,'control_session')!=session or not session or
                uint(row,'slot')!=0 or uint(row,'generation')!=generation or not generation or
                next_sequence<=sequence):
            raise RuntimeError('Feedback session/generation/sequence mismatch')
        if (uint(row,'kind')!=1 or uint(row,'byte_size')!=48 or uint(row,'report_id')!=1 or
                row.get('hid_output_dequeued') is not True or row.get('sdk_dispatched') is not False or
                row.get('physical_device_verified') is not False):
            raise RuntimeError('Malformed or unproven HID feedback record')
        uint(row,'left',65535);uint(row,'right',65535)
        # Retain the established per-record evidence format in summary.json.
        checked.append({**row,'event':'wine_input_feedback','ok':True,'win32_error':0,'available':True})
        sequence=next_sequence
    return checked,sequence

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--wine',required=True,type=pathlib.Path)
    parser.add_argument('--arch',required=True,choices=['arm64','x64'])
    parser.add_argument('--consumer-arch',choices=['arm64','x64','x86'])
    parser.add_argument('--prefix',required=True,type=pathlib.Path)
    parser.add_argument('--log-dir',required=True,type=pathlib.Path)
    args=parser.parse_args();prefix=args.prefix.resolve();logs=args.log_dir.resolve()
    payload=ROOT/'target/wine-input-rumble-experiment';consumer_arch=args.consumer_arch or args.arch
    if prefix.exists():raise RuntimeError('Refusing existing prefix')
    if logs.exists() and any(logs.iterdir()):raise RuntimeError('Refusing nonempty logs')
    if not args.wine.is_file():raise RuntimeError('Wine loader missing')
    manifest=json.loads((payload/'manifest.json').read_text())
    if manifest.get('experiment')!='owned_wine_haptics_feedback':raise RuntimeError('Unexpected experimental payload')
    selected=[args.arch+'/'+n for n in ['feeder.exe','skinosinput.sys','skinosinput.inf','consumer.exe']]
    selected+=[consumer_arch+'/rumble-consumer.exe']
    for name in selected:
        path=payload/name
        if path.is_symlink() or hashlib.sha256(path.read_bytes()).hexdigest()!=manifest['files'][name]:raise RuntimeError('Artifact checksum mismatch: '+name)
    descriptor=bytes(int(n,16) for n in re.findall(r'0x([0-9a-f]{2})',(ROOT/'third_party/notproton/integration/wine-input/descriptor.h').read_text()))
    if hashlib.sha256(descriptor).hexdigest()!=base.DESCRIPTOR_SHA:raise RuntimeError('Original producer descriptor mismatch')
    logs.mkdir(parents=True,exist_ok=True);prefix.mkdir(parents=True,mode=0o700)
    (logs/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    env=os.environ.copy();env.update(WINEPREFIX=str(prefix),WINEARCH='win64',WINELOADER=str(args.wine.absolute()))
    for key in list(env):
        if key.startswith(('Steam','STEAM_','SKINOS_')) or key in ['DYLD_INSERT_LIBRARIES','WINEDLLOVERRIDES']:env.pop(key)
    def record(event,**fields):
        with (logs/'timeline.jsonl').open('a') as f:f.write(json.dumps({'event':event,'time_ns':time.monotonic_ns(),**fields})+'\n')
    def run(name,arguments,timeout=20):
        with (logs/(name+'.stdout.log')).open('wb') as out,(logs/(name+'.stderr.log')).open('wb') as err:
            result=subprocess.run([str(args.wine.absolute())]+arguments,env=env,stdout=out,stderr=err,timeout=timeout)
        return result.returncode
    def windows(relative):return str(pathlib.PureWindowsPath('C:/SkinOSRumbleQA')/relative)
    feeder=input_consumer=rumble=None;attempted=False;results=[];failure=None;feedback=[];session=None;last_sequence=0
    summary={'transport_only':True,'steam_input_verified':False,'physical_device_verified':False,'passed':False,'prefix':str(prefix)}
    try:
        if run('prefix-init',['cmd.exe','/c','exit','0'],60):raise RuntimeError('Automatic prefix init failed')
        # Execute only verified copies inside this fresh prefix. Do not depend
        # on a Z: path through a host/sandbox directory at launch time.
        stage=prefix/'drive_c/SkinOSRumbleQA'
        for name in selected:
            dest=stage/name;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(payload/name,dest)
            if hashlib.sha256(dest.read_bytes()).hexdigest()!=manifest['files'][name]:raise RuntimeError('Staging checksum mismatch')
        record('staged',files=selected)
        attempted=True
        feeder=base.Child([str(args.wine.absolute()),windows(args.arch+'/feeder.exe'),'--ensure-feedback',windows(args.arch+'/skinosinput.inf')],env,logs,'feeder')
        if feeder.wait('wine_input_ready',45).get('ok') is not True:raise RuntimeError('Feeder not ready')
        input_consumer=base.Child([str(args.wine.absolute()),windows(args.arch+'/consumer.exe')],env,logs,'input-consumer')
        input_consumer.wait('transport_consumer_ready')
        def input_compare(label,value=None):
            input_consumer.send(base.frame(3) if value is None else base.frame(2,value))
            row=input_consumer.wait('transport_comparison',8);row['case']=label;results.append(row)
            if row.get('ok') is not True:raise RuntimeError('Input regression: '+label)
        bindings=[]
        def submit(kind,value=b''):
            feeder.send(base.frame(kind,value));ack=feeder.wait('wine_input_ack')
            if ack.get('ok') is not True or ack.get('kind')!=kind:raise RuntimeError('Input frame rejected: '+str(ack))
            if kind==1:
                if ack.get('driver_binding_verified') is not True or not ack.get('control_session') or not ack.get('generation'):raise RuntimeError('Missing authoritative driver binding')
                bindings.append(ack);record('driver_create_binding',binding=ack)
        input_compare('empty_baseline')
        submit(1,descriptor);input_compare('create_only_neutral',base.report())
        # Whole established input matrix ensures output IDs did not shift input.
        for label,value in base.cases():submit(2,value);input_compare(label,value)
        rumble=base.Child([str(args.wine.absolute()),windows(consumer_arch+'/rumble-consumer.exe')],env,logs,'rumble-consumer')
        rumble.wait('wine_rumble_consumer_ready')
        def inspect():
            rumble.send(base.frame(6));row=rumble.wait('wine_rumble_result',8)
            record('inspect',result=row)
            if row.get('ok') is not True or row.get('owned_slot')!=0 or not row.get('generation'):raise RuntimeError('Actual HID parser/device inspection failed')
            if [row.get(k) for k in ['input_bytes','output_bytes','feature_bytes']]!=[15,5,17]:raise RuntimeError('HID report lengths differ')
            return row
        identity=inspect();generation=identity['generation']
        if generation!=bindings[-1]['generation']:raise RuntimeError('HID path and authoritative driver generation differ')
        session=bindings[-1]['control_session']
        def poll_feedback():
            nonlocal last_sequence
            feeder.send(base.frame(4));envelope=feeder.wait('wine_input_feedback')
            rows,next_sequence=decode_feedback(envelope,session,generation,last_sequence)
            last_sequence=next_sequence
            for row in rows:
                feedback.append(row);record('actual_hid_feedback',result=row)
            return rows
        # Permit only genuine initialization zeroes before an explicit request.
        for _ in range(65):
            rows=poll_feedback()
            if not rows:break
            if any(row['left'] or row['right'] for row in rows):raise RuntimeError('Unexpected initial motor request')
        else:raise RuntimeError('Feedback baseline never drained')
        for left,right in [(0x1234,0),(0,0),(0,0xabcd),(0,0),(65535,65535),(0,0)]:
            rumble.send(base.frame(5,struct.pack('<HH',left,right)))
            result=rumble.wait('wine_rumble_result',8);results.append(result);record('xinput_setstate',result=result)
            if result.get('ok') is not True or result.get('xinput_result')!=0 or result.get('xinput_set_called') is not True:raise RuntimeError('XInput command/ownership failed')
            if result.get('generation')!=generation:raise RuntimeError('Consumer changed device generation')
            until=time.monotonic()+3
            while True:
                rows=poll_feedback()
                if rows:
                    if any((row['left'],row['right'])!=(left,right) for row in rows):raise RuntimeError('HID feedback motor values differ from XInput request')
                    break
                if time.monotonic()>=until:raise RuntimeError('XInput returned success without an actual HID output')
                time.sleep(.025)
        # An unread output for a removed generation must not reach a replacement.
        rumble.send(base.frame(5,struct.pack('<HH',0x2222,0)))
        queued=rumble.wait('wine_rumble_result',8)
        if queued.get('ok') is not True:raise RuntimeError('Removal fixture command failed')
        # Leave both a nonzero and its explicit zero unread in the driver.
        # This tests stale-generation purge without asking the consumer to
        # report a successful stop after its verified device has disappeared.
        rumble.send(base.frame(5,struct.pack('<HH',0,0)))
        zero=rumble.wait('wine_rumble_result',8)
        if zero.get('ok') is not True:raise RuntimeError('Removal fixture zero command failed')
        submit(3);input_compare('detached')
        stale=poll_feedback()
        if stale:raise RuntimeError('Removed PDO feedback survived purge')
        if rumble.close():raise RuntimeError('Retired-generation rumble consumer exit failed')
        rumble=None
        submit(1,descriptor);input_compare('recreated_neutral',base.report())
        rumble=base.Child([str(args.wine.absolute()),windows(consumer_arch+'/rumble-consumer.exe')],env,logs,'rumble-recreated')
        rumble.wait('wine_rumble_consumer_ready')
        identity=inspect()
        if identity['generation']==generation:raise RuntimeError('Recreate reused old PDO generation')
        if identity['generation']!=bindings[-1]['generation'] or session!=bindings[-1]['control_session']:raise RuntimeError('Recreated driver/path/session binding differs')
        generation=identity['generation'];summary['recreated_generation']=generation
        submit(3);input_compare('final_detached')
        summary.update(hid_feedback_verified=True,original_generation=feedback[0]['generation'] if feedback else None)
    except BaseException as exc:
        failure=exc;summary.update(error_type=type(exc).__name__,error=str(exc))
    finally:
        cleanup={'errors':[],'attempted':attempted}
        if feeder:
            try:
                code=feeder.close();cleanup['feeder_exit']=code
                if code:cleanup['errors'].append('Feeder exit failure')
            except Exception as exc:cleanup['errors'].append('feeder: '+str(exc))
        if attempted:
            try:
                cleanup['uninstall_exit']=run('uninstall',[windows(args.arch+'/feeder.exe'),'--uninstall'])
                removed=False;deadline=time.monotonic()+5
                for attempt in range(10):
                    name='cleanup-after-'+str(attempt);run(name,[windows(args.arch+'/feeder.exe'),'--status'],5)
                    rows=[]
                    for line in (logs/(name+'.stdout.log')).read_text(errors='replace').splitlines():
                        try:rows.append(json.loads(line))
                        except json.JSONDecodeError:pass
                    state=next((x for x in rows if x.get('event')=='wine_input_driver_status'),{})
                    removed=state.get('service_found') is False and state.get('service_query_error')==1060 and state.get('cm_locate_result')==13
                    cleanup['final_status']=state
                    if removed or time.monotonic()>=deadline:break
                    time.sleep(.25)
                cleanup['removed']=removed
                if not removed or cleanup['uninstall_exit']:cleanup['errors'].append('Owned root/service cleanup not confirmed')
            except Exception as exc:cleanup['errors'].append('uninstall: '+str(exc))
        for name,child in [('rumble',rumble),('input_consumer',input_consumer)]:
            if child:
                try:
                    code=child.close();cleanup[name+'_exit']=code
                    if code:cleanup['errors'].append(name+' exit failure')
                except Exception as exc:cleanup['errors'].append(name+': '+str(exc))
        summary.update(results=results,feedback=feedback,cleanup=cleanup,passed=failure is None and not cleanup['errors'])
        (logs/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    if failure:raise failure
    if not summary['passed']:raise RuntimeError('Rumble transport cleanup failed')
    print(json.dumps({'passed':True,'hid_feedback_verified':True,'steam_input_verified':False,'summary':str(logs/'summary.json')}))

if __name__=='__main__':main()
