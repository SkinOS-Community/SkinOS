#!/usr/bin/env python3
"""Exercise the user's requested Steam downloads, then leave them paused.
No tokens, cookie values, CDN URLs or chat contents are printed or saved.
"""
import argparse,json,queue,subprocess,threading,time
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--core',type=Path,required=True);p.add_argument('--timeout',type=float,default=240);args=p.parse_args()
appids=[2073850,648800]
proc=subprocess.Popen([str(args.core)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True)
inbox=queue.Queue()
def reader():
 for line in proc.stdout:
  try:inbox.put(json.loads(line))
  except json.JSONDecodeError:pass
 inbox.put(None)
threading.Thread(target=reader,daemon=True).start()
seq=0;latest=None;steamid=None;authed=False;live=False;proof={'apps':appids,'checks':[]};started=time.monotonic()
def send(command,**payload):
 global seq
 seq+=1;rid=f'{command}-{seq}';proc.stdin.write(json.dumps({'id':rid,'command':command,'payload':payload})+'\n');proc.stdin.flush();return rid

def receive(timeout=1):
 global latest,authed,live,steamid
 try:e=inbox.get(timeout=timeout)
 except queue.Empty:return None
 if e is None:raise RuntimeError('Core closed its event stream')
 kind=e.get('event');payload=e.get('payload',{})
 if kind=='ready':send('status')
 elif kind=='status' and (e.get('id') or '').startswith('status-'):
  accounts=payload.get('accounts',[])
  if not accounts:raise RuntimeError('No saved account')
  steamid=accounts[0]['steamid'];send('restore_session',steamid=steamid)
 elif kind=='authenticated':authed=True
 elif kind=='library' and not payload.get('cached',True):
  ids={g['appid'] for g in payload.get('games',[])}
  if not set(appids)<=ids:raise RuntimeError('Requested games are missing from live owned library')
  live=True
 elif kind=='download_queue':
  if payload.get('steamid',steamid)==steamid and (latest is None or payload.get('revision',0)>=latest.get('revision',0)):latest=payload
 elif kind=='error':
  # Only errors correlated to this test's explicit requests are fatal here.
  if e.get('id'):raise RuntimeError(payload.get('message','Backend request failed'))
 return e

def items():return {x['appid']:x for x in (latest or {}).get('items',[]) if x['appid'] in appids}
def wait(label,predicate):
 deadline=min(started+args.timeout,time.monotonic()+args.timeout)
 while time.monotonic()<deadline:
  if predicate():proof['checks'].append(label);print(label,flush=True);return
  receive()
 raise RuntimeError('Timed out: '+label)

def snapshot():
 return [{k:x.get(k) for k in ['appid','state','downloaded_bytes','total_bytes','reused_bytes','processed_bytes','install_total_bytes','queue_position']} for x in items().values()]

failure=None
try:
 wait('authenticated_live_library',lambda:authed and live)
 send('get_download_queue')
 wait('restored_queue_received',lambda:latest is not None)
 initial_bytes={appid:items().get(appid,{}).get('downloaded_bytes',0) for appid in appids}
 send('set_download_limit',limit=2)
 for appid in appids:
  state=items().get(appid,{}).get('state')
  if state in ['paused','cancelled','failed']:send('resume_download',appid=appid)
  elif state not in ['queued','preparing','verifying','downloading','installing']:send('install_game',appid=appid)
 wait('two_parallel_jobs',lambda:len(items())==2 and all(x['state'] in ['preparing','verifying','downloading','installing'] for x in items().values()))
 wait('both_receive_network_bytes',lambda:len(items())==2 and all(x['state']=='downloading' and x.get('downloaded_bytes',0)>=initial_bytes[k]+256*1024 for k,x in items().items()))
 proof['before_pause']=snapshot()
 for appid in appids:send('pause_download',appid=appid)
 wait('both_paused',lambda:len(items())==2 and all(x['state']=='paused' for x in items().values()))
 before={k:v.get('downloaded_bytes',0) for k,v in items().items()}
 end=time.monotonic()+1
 while time.monotonic()<end:receive(.1)
 send('get_download_queue')
 end=time.monotonic()+.5
 while time.monotonic()<end:receive(.1)
 assert all(x['state']=='paused' and x.get('downloaded_bytes',0)==before[k] for k,x in items().items()),'Paused transfer still changes'
 proof['checks'].append('paused_transfer_is_stable')
 send('resume_download',appid=648800)
 wait('resumed_network_transfer',lambda:items().get(648800,{}).get('state')=='downloading' and items()[648800].get('downloaded_bytes',0)>before[648800])
 send('pause_download',appid=648800)
 wait('resumed_job_paused_again',lambda:items().get(648800,{}).get('state')=='paused')
 send('cancel_download',appid=648800)
 wait('cancelled_job',lambda:items().get(648800,{}).get('state')=='cancelled')
 send('resume_download',appid=648800)
 wait('cancelled_job_can_retry',lambda:items().get(648800,{}).get('state') in ['queued','preparing','verifying','downloading'])
 send('pause_download',appid=648800)
 wait('final_jobs_paused',lambda:len(items())==2 and all(x['state']=='paused' for x in items().values()))
 proof['final']=snapshot();proof['passed']=True
except Exception as e:
 failure=str(e);proof.update(passed=False,error=failure,final=snapshot());print('FAILED',failure,flush=True)
finally:
 if proc.poll() is None:
  try:send('quit');proc.wait(timeout=20)
  except (BrokenPipeError,subprocess.TimeoutExpired):
   proc.terminate();proc.wait(timeout=5)
 proof['core_exit_code']=proc.returncode;proof['elapsed_seconds']=round(time.monotonic()-started,2)
 Path('outputs/download-queue-live-check.json').write_text(json.dumps(proof,indent=2))
if failure:raise SystemExit(1)
