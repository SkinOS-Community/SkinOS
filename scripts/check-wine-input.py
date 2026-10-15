#!/usr/bin/env python3
"""Explicit isolated Wine transport conformance. This is NOT Steam Input proof.

Refuses an existing prefix. No real controller, Steam SDK, login, or game is used.
Only call when an isolated Wine driver installation/test was authorized.
"""
import argparse
import hashlib
import json
import os
import pathlib
import queue
import struct
import subprocess
import threading
import time

ROOT=pathlib.Path(__file__).resolve().parents[1]
MAGIC=0x53494e50
DESCRIPTOR_SHA='086699843817c3d3554b4de8d239d809407b328eafa4a82aae93284f669524a7'

def frame(kind,payload=b'',slot=0):
    if len(payload)>256:raise ValueError('oversized payload')
    return struct.pack('<8I',MAGIC,1,kind,slot,len(payload),0x045e,0x028e,0)+payload.ljust(256,b'\0')

def report(buttons=0,lt=0,rt=0,axes=(0,0,0,0)):
    return struct.pack('<2xHBB4h6x',buttons,lt,rt,*axes)

def cases():
    yield 'neutral',report()
    for bit in range(16):yield 'button_'+str(bit),report(1<<bit)
    for dpad in range(16):yield 'dpad_'+str(dpad),report(dpad)
    for lt in [0,255]:
        for rt in [0,255]:yield f'triggers_{lt}_{rt}',report(lt=lt,rt=rt)
    for axis in range(4):
        for value in [-32768,0,32767]:
            values=[0]*4;values[axis]=value;yield f'axis_{axis}_{value}',report(axes=values)
    yield 'combined',report(0x5350,127,255,(-32768,32767,32767,-32768))
    yield 'release',report()

def selected_consumer(payload,driver_arch,consumer_arch=None,consumer_path=None):
    """Only execute a named, hash-pinned consumer from our owned build roots."""
    arch=consumer_arch or driver_arch
    roots=[(payload.resolve(),'consumer.exe'),((ROOT/'target/wine-input-di-experiment').resolve(),'consumer-dinput.exe')]
    requested=consumer_path if consumer_path is not None else payload/arch/'consumer.exe'
    if requested.is_symlink():raise RuntimeError('Consumer file must not be a symlink')
    selected=requested.resolve()
    match=next(((root,name) for root,name in roots if selected==root/arch/name),None)
    if match is None:raise RuntimeError('Consumer path/architecture must match an owned payload manifest')
    root,name=match;manifest=json.loads((root/'manifest.json').read_text());data=selected.read_bytes()
    digest=hashlib.sha256(data).hexdigest()
    if digest!=manifest['files'].get(arch+'/'+name):raise RuntimeError('Consumer checksum mismatch')
    if len(data)<64 or data[:2]!=b'MZ':raise RuntimeError('Consumer is not a PE image')
    offset=struct.unpack_from('<I',data,0x3c)[0]
    if offset+6>len(data) or data[offset:offset+4]!=b'PE\0\0':raise RuntimeError('Consumer PE header is invalid')
    machine=struct.unpack_from('<H',data,offset+4)[0]
    if machine!={'x64':0x8664,'arm64':0xaa64,'x86':0x014c}[arch]:raise RuntimeError('Consumer PE architecture mismatch')
    return selected,arch,digest

class Child:
    def __init__(self,command,env,log,name):
        self.name=name;self.log=log;self.lines=queue.Queue();self.stderr=open(log/(name+'.stderr.log'),'wb')
        self.stdout=open(log/(name+'.stdout.jsonl'),'w')
        self.process=subprocess.Popen(command,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=self.stderr)
        def reader():
            for raw in self.process.stdout:
                line=raw.decode('utf-8','replace');self.stdout.write(line);self.stdout.flush()
                try:self.lines.put(json.loads(line))
                except json.JSONDecodeError:continue
            self.lines.put(None)
        self.thread=threading.Thread(target=reader,daemon=True);self.thread.start()
    def wait(self,event,timeout=12):
        until=time.monotonic()+timeout
        while True:
            item=self.lines.get(timeout=max(0.01,until-time.monotonic()))
            if item is None:raise RuntimeError(self.name+' exited before '+event)
            if item.get('event')==event:return item
            if item.get('ok') is False:raise RuntimeError(str(item))
            if time.monotonic()>=until:raise TimeoutError(event)
    def send(self,data):self.process.stdin.write(data);self.process.stdin.flush()
    def close(self):
        if self.process.stdin and not self.process.stdin.closed:self.process.stdin.close()
        try:code=self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            try:code=self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:self.process.kill();code=self.process.wait(timeout=3)
        self.thread.join(timeout=2);self.stderr.close();self.stdout.close();return code

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--wine',required=True,type=pathlib.Path)
    parser.add_argument('--prefix',required=True,type=pathlib.Path)
    parser.add_argument('--arch',choices=['x64','arm64'],required=True)
    parser.add_argument('--payload',type=pathlib.Path,default=ROOT/'target/wine-input')
    parser.add_argument('--consumer-arch',choices=['x64','arm64','x86'],help='Consumer architecture; does not change driver/feeder architecture')
    parser.add_argument('--consumer-path',type=pathlib.Path,help='Explicit consumer from an owned payload root, checked against its manifest')
    parser.add_argument('--ensure-feeder',action='store_true',help='Install, activate and feed in one owned Wine process')
    parser.add_argument('--log-dir',type=pathlib.Path,required=True)
    parser.add_argument('--frames-only',action='store_true',help='Write test frames only, without Wine or installation')
    args=parser.parse_args();folder=args.payload.resolve()/args.arch;logs=args.log_dir.resolve()
    logs.mkdir(parents=True,exist_ok=True)
    descriptor=bytes(int(x,16) for x in __import__('re').findall(r'0x([0-9a-f]{2})',
        (ROOT/'third_party/notproton/integration/wine-input/descriptor.h').read_text()))
    if len(descriptor)!=188 or hashlib.sha256(descriptor).hexdigest()!=DESCRIPTOR_SHA:raise RuntimeError('descriptor mismatch')
    if args.frames_only:
        (logs/'transport-fixture.frames').write_bytes(frame(1,descriptor)+b''.join(frame(2,r) for _,r in cases())+frame(3))
        print(json.dumps({'transport_only':True,'frames_written':True,'wine_executed':False}));return
    prefix=args.prefix.resolve()
    if prefix.exists():raise RuntimeError('Refusing existing prefix: '+str(prefix))
    if not args.wine.resolve().is_file():raise RuntimeError('Wine loader not found')
    manifest=json.loads((args.payload/'manifest.json').read_text())
    (logs/'payload-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    for name in ['feeder.exe','skinosinput.sys','skinosinput.inf']:
        if hashlib.sha256((folder/name).read_bytes()).hexdigest()!=manifest['files'][args.arch+'/'+name]:raise RuntimeError('payload checksum '+name)
    consumer_path,consumer_arch,consumer_sha=selected_consumer(args.payload,args.arch,args.consumer_arch,args.consumer_path)
    prefix.mkdir(parents=True,mode=0o700)
    env=os.environ.copy();env['WINEPREFIX']=str(prefix);env['WINEARCH']='win64';env['WINELOADER']=str(args.wine.absolute())
    # No inherited Steam launch/auth or injection context enters this fixture.
    for key in list(env):
        if key.startswith(('Steam','STEAM_','SKINOS_')) or key in ['DYLD_INSERT_LIBRARIES','WINEDLLOVERRIDES']:env.pop(key)
    def windows(path):return 'Z:'+str(path.resolve()).replace('/','\\')
    def run(name,arguments,timeout=60,checked=True):
        with open(logs/(name+'.stdout.log'),'wb') as stdout,open(logs/(name+'.stderr.log'),'wb') as stderr:
            result=subprocess.run([str(args.wine.absolute())]+arguments,env=env,stdout=stdout,stderr=stderr,timeout=timeout)
        if checked and result.returncode:raise RuntimeError(name+' failed: '+str(result.returncode))
        return result.returncode
    feeder=consumer=None;install_attempted=False;results=[];failure=None;summary=None
    try:
        # Wine's implicit first-prefix initialization already runs wineboot.
        # Starting explicit --init again created two concurrent SCM instances
        # in the selected FEX runner (captured in fex-7/8 service traces).
        run('prefix-init',['cmd.exe','/c','exit','0'])
        feed_args = ['--ensure-and-feed',windows(folder/'skinosinput.inf')] if args.ensure_feeder else ['--feed']
        if args.ensure_feeder:
            # No separate keeper: one Wine process owns setup through EOF.
            install_attempted=True
            feeder=Child([str(args.wine.absolute()),windows(folder/'feeder.exe')]+feed_args,env,logs,'feeder')
            feeder.wait('wine_input_ready',timeout=45)
            consumer=Child([str(args.wine.absolute()),windows(consumer_path)],env,logs,'consumer')
            ready=consumer.wait('transport_consumer_ready')
        else:
            consumer=Child([str(args.wine.absolute()),windows(consumer_path)],env,logs,'consumer')
            ready=consumer.wait('transport_consumer_ready')
            install_attempted=True
            run('register',[windows(folder/'feeder.exe'),'--register',windows(folder/'skinosinput.inf')])
            run('activate',[windows(folder/'feeder.exe'),'--activate'])
            feeder=Child([str(args.wine.absolute()),windows(folder/'feeder.exe')]+feed_args,env,logs,'feeder')
            feeder.wait('wine_input_ready')
        consumer.send(frame(3));baseline=consumer.wait('transport_comparison');baseline['case']='empty_baseline';results.append(baseline)
        if not baseline.get('ok'):raise RuntimeError('Fixture requires no pre-existing XInput/DirectInput pads')
        feeder.send(frame(1,descriptor));ack=feeder.wait('wine_input_ack')
        if not ack.get('ok'):raise RuntimeError(str(ack))
        consumer.send(frame(2,report()));initial=consumer.wait('transport_comparison');initial['case']='create_only_neutral';results.append(initial)
        if not initial.get('ok'):raise RuntimeError('Newly created device did not expose neutral state')
        feeder.send(frame(2,report()));ack=feeder.wait('wine_input_ack')
        if not ack.get('ok'):raise RuntimeError(str(ack))
        for label,value in cases():
            feeder.send(frame(2,value));ack=feeder.wait('wine_input_ack')
            if not ack.get('ok'):raise RuntimeError(str(ack))
            consumer.send(frame(2,value));result=consumer.wait('transport_comparison');result['case']=label;results.append(result)
            print(json.dumps({'case':label,'ok':result.get('ok'),'transport_only':True}),flush=True)
            if not result.get('ok'):raise RuntimeError('Transport mismatch: '+label)
        feeder.send(frame(3));ack=feeder.wait('wine_input_ack')
        if not ack.get('ok'):raise RuntimeError(str(ack))
        consumer.send(frame(3));result=consumer.wait('transport_comparison');result['case']='detach';results.append(result)
        if not result.get('ok'):raise RuntimeError('Detach mismatch')
        if feeder.close():raise RuntimeError('Feeder cleanup failed')
        feeder=None
        # Separate CLOSE / EOF regression: reacquire the control connection,
        # prove an empty baseline, hold real fixture state, then close stdin
        # without a Destroy frame. Nothing may remain latched or enumerated.
        feeder=Child([str(args.wine.absolute()),windows(folder/'feeder.exe')]+feed_args,env,logs,'feeder-reopen')
        feeder.wait('wine_input_ready')
        consumer.send(frame(3));result=consumer.wait('transport_comparison');result['case']='reopen_empty_baseline';results.append(result)
        if not result.get('ok'):raise RuntimeError('Reopened feeder inherited a stale device')
        feeder.send(frame(1,descriptor));ack=feeder.wait('wine_input_ack')
        if not ack.get('ok'):raise RuntimeError(str(ack))
        # Reacquiring DirectInput resets its per-consumer state and begins a
        # new asynchronous read. Establish actual neutral/readiness before
        # the held-state transition; EOF cleanup must not be confounded with
        # replaying an event emitted before this consumer acquired the device.
        consumer.send(frame(2,report()));result=consumer.wait('transport_comparison');result['case']='reopen_neutral_ready';results.append(result)
        if not result.get('ok'):raise RuntimeError('Reopened device consumer did not become neutral-ready')
        held=report(buttons=0x1010,lt=255,axes=(32767,-32768,1234,-2345))
        feeder.send(frame(2,held));ack=feeder.wait('wine_input_ack')
        if not ack.get('ok'):raise RuntimeError(str(ack))
        consumer.send(frame(2,held));result=consumer.wait('transport_comparison');result['case']='forced_eof_held_state';results.append(result)
        if not result.get('ok'):raise RuntimeError('Held-state setup before EOF failed')
        if feeder.close():raise RuntimeError('Forced EOF cleanup failed')
        feeder=None
        consumer.send(frame(3));result=consumer.wait('transport_comparison');result['case']='forced_eof_removed_state';results.append(result)
        if not result.get('ok'):raise RuntimeError('EOF left a held or enumerated device')
        feeder=Child([str(args.wine.absolute()),windows(folder/'feeder.exe')]+feed_args,env,logs,'feeder-after-eof')
        feeder.wait('wine_input_ready')
        consumer.send(frame(3));result=consumer.wait('transport_comparison');result['case']='reopen_after_forced_eof';results.append(result)
        if not result.get('ok'):raise RuntimeError('Control CLOSE prevented clean reopen')
        if feeder.close():raise RuntimeError('Post-EOF close failed')
        feeder=None
        summary={'transport_only':True,'steam_input_verified':False,'physical_device_verified':False,
            'passed':True,'cases':results,'xinput_extended':ready.get('xinput_extended'),'prefix':str(prefix),
            'ensure_feeder':args.ensure_feeder,'driver_arch':args.arch,'consumer_arch':consumer_arch,'consumer_path':str(consumer_path),'consumer_sha256':consumer_sha}
    except BaseException as exc:
        failure={'transport_only':True,'steam_input_verified':False,'passed':False,
            'error_type':type(exc).__name__,'error':str(exc),'cases':results,'prefix':str(prefix)}
        (logs/'failure.json').write_text(json.dumps(failure,indent=2)+'\n')
        raise
    finally:
        cleanup={'install_attempted':install_attempted,'diagnostics':{},'errors':[]}
        # Feeder closes first to remove all PDOs; consumer remains alive until
        # the uninstall/readback boundary has completed.
        for name,child in [('feeder',feeder)]:
            if child:
                try:cleanup[name+'_exit']=child.close()
                except Exception as exc:cleanup['errors'].append(name+': '+str(exc))
        if install_attempted:
            def cleanup_command(name,arguments,timeout=15):
                try:
                    cleanup[name+'_exit']=run(name,arguments,timeout=timeout,checked=False)
                    lines=[]
                    for line in (logs/(name+'.stdout.log')).read_text(errors='replace').splitlines():
                        try:lines.append(json.loads(line))
                        except json.JSONDecodeError:pass
                    cleanup['diagnostics'][name]=lines
                    return next((line for line in reversed(lines) if line.get('event')=='wine_input_driver_status'),None)
                except Exception as exc:cleanup['errors'].append(name+': '+str(exc))
                return None
            cleanup_command('cleanup-before',[windows(folder/'feeder.exe'),'--status'])
            cleanup_command('uninstall',[windows(folder/'feeder.exe'),'--uninstall'])
            deadline=time.monotonic()+5;attempt=0;removed=False
            while True:
                attempt+=1;name='cleanup-after' if attempt==1 else 'cleanup-after-'+str(attempt)
                state=cleanup_command(name,[windows(folder/'feeder.exe'),'--status'],timeout=3)
                service_absent=state is not None and state.get('service_found') is False and state.get('service_query_error')==1060
                root_absent=state is not None and state.get('cm_locate_result')==13
                cleanup['service_absent_verified']=service_absent;cleanup['root_absent_verified']=root_absent
                removed=service_absent and root_absent
                if removed or time.monotonic()>=deadline or attempt>=10:break
                time.sleep(0.25)
            cleanup['removal_state']='removed' if removed else 'pending_or_failed'
            if not removed:cleanup['errors'].append('Owned service/root removal was not confirmed within bounded readback')
        if consumer:
            try:cleanup['consumer_exit']=consumer.close()
            except Exception as exc:cleanup['errors'].append('consumer: '+str(exc))
        (logs/'cleanup.json').write_text(json.dumps(cleanup,indent=2)+'\n')
        if summary is not None:
            summary['cleanup']=cleanup
            if cleanup['errors'] or cleanup.get('uninstall_exit',0)!=0 or cleanup.get('consumer_exit',0)!=0:summary['passed']=False
            (logs/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
            if not summary['passed'] and failure is None:raise RuntimeError('Transport checks passed but cleanup failed; see cleanup.json')

if __name__=='__main__':main()
