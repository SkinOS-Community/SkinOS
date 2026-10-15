#!/usr/bin/env python3
"""Offline test of actual experimental feeder loop: query error must preserve input."""
import hashlib
import importlib.util
import json
import pathlib
import subprocess

ROOT=pathlib.Path(__file__).resolve().parents[1]
SOURCE=ROOT/'third_party/notproton/integration/wine-input/rumble-experiment'
OUT=ROOT/'target/wine-feedback-query-tests'
spec=importlib.util.spec_from_file_location('extract_driver_test',ROOT/'scripts/test-wine-input-replay.py')
extractor=importlib.util.module_from_spec(spec);spec.loader.exec_module(extractor)
PRELUDE=r'''
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include "protocol.h"
#include "descriptor.h"
typedef uint32_t DWORD;typedef int BOOL;typedef uint8_t BYTE;typedef uintptr_t HANDLE;
#define TRUE 1
#define FALSE 0
#define STD_INPUT_HANDLE 1
#define ERROR_BROKEN_PIPE 109
#define ERROR_INVALID_DATA 13
#define ERROR_INVALID_STATE 5023
#define ERROR_GEN_FAILURE 31
static BOOL feedback_mode,owned[SKINOS_INPUT_SLOTS];
static SkinosInputFrame rows[3];static unsigned row_index,submitted,queries,failed_replies,closed,cleaned,acknowledged;
static HANDLE GetStdHandle(int type){assert(type==STD_INPUT_HANDLE);return 1;}
static BOOL ReadFile(HANDLE file,void*buffer,DWORD count,DWORD*read,void*overlapped){
 assert(file==1&&overlapped==NULL&&count==sizeof(SkinosInputFrame));
 if(row_index==3){*read=0;return TRUE;}memcpy(buffer,&rows[row_index++],count);*read=count;return TRUE;
}
static DWORD GetLastError(void){return 0;}
static BOOL CloseHandle(HANDLE bus){assert(bus==42);++closed;return TRUE;}
static BOOL wait_root(HANDLE bus,DWORD timeout,DWORD*error){assert(bus==42&&timeout==5000);*error=0;return TRUE;}
static void diagnose(void){}
static int same(const void*a,const void*b,size_t bytes){return !memcmp(a,b,bytes);}
static void event(const char*name,BOOL ok,DWORD error,DWORD slot,DWORD kind){
 (void)slot;(void)kind;
 if(!strcmp(name,"wine_input_ack")){assert(ok&&error==0);++acknowledged;}
 if(!strcmp(name,"wine_input_feedback")){assert(!ok&&error==5);++failed_replies;}
}
static BOOL submit(HANDLE bus,const SkinosInputFrame*frame,DWORD*error){
 assert(bus==42&&(frame->kind==1||frame->kind==2));assert(frame->kind==submitted+1);++submitted;*error=0;return TRUE;
}
static BOOL cleanup(HANDLE bus){assert(bus==42);++cleaned;return TRUE;}
static DWORD feedback_read(HANDLE bus){assert(bus==42);++queries;event("wine_input_feedback",FALSE,5,0,0);return 5;}
static BOOL feedback_create_ack(HANDLE bus,DWORD slot,DWORD*error){assert(bus==42&&slot==0);*error=0;event("wine_input_ack",TRUE,0,slot,1);return TRUE;}
'''
TEST=r'''
static void reset(void){
 memset(rows,0,sizeof(rows));memset(owned,0,sizeof(owned));row_index=submitted=queries=failed_replies=closed=cleaned=acknowledged=0;feedback_mode=TRUE;
 for(unsigned i=0;i<3;i++){rows[i].magic=SKINOS_INPUT_MAGIC;rows[i].version=1;}
 rows[0].kind=4;
 rows[1].kind=1;rows[1].payload_len=sizeof(skinos_input_descriptor);memcpy(rows[1].payload,skinos_input_descriptor,sizeof(skinos_input_descriptor));
 rows[2].kind=2;rows[2].payload_len=20;
}
int main(void){
 reset();assert(feed_handle(42)==0);assert(queries==1&&failed_replies==1&&submitted==2&&acknowledged==2&&owned[0]&&closed==1&&cleaned==1);
 reset();rows[0].payload_len=1;assert(feed_handle(42)==ERROR_INVALID_DATA);assert(queries==0&&submitted==0&&closed==1);
 reset();feedback_mode=FALSE;assert(feed_handle(42)==ERROR_INVALID_DATA);assert(queries==0&&submitted==0&&closed==1);
 puts("OFFLINE ONLY: actual feeder loop preserves Create/Report after failed query; malformed query and default feed remain strict (3 cases)");
}
'''

def main():
    text=(SOURCE/'feeder.c').read_text()
    functions='\n'.join(extractor.extract_function(text,name) for name in ['valid','feed_handle'])
    OUT.mkdir(parents=True,exist_ok=True)
    source=OUT/'feeder-loop-test.c';source.write_text(PRELUDE+'\n'+functions+'\n'+TEST)
    executable=OUT/'feeder-loop-test'
    subprocess.run(['clang','-std=c11','-O1','-g','-Wall','-Wextra','-Werror','-fsanitize=address,undefined',
                    '-I',str(SOURCE),str(source),'-o',str(executable)],check=True)
    result=subprocess.run([str(executable)],check=True,text=True,capture_output=True)
    (OUT/'report.json').write_text(json.dumps({'offline_only':True,'cases_passed':3,
        'source_sha256':hashlib.sha256(text.encode()).hexdigest(),'extracted_sha256':hashlib.sha256(functions.encode()).hexdigest(),
        'stdout':result.stdout,'stderr':result.stderr},indent=2)+'\n')
    print(result.stdout,end='')

if __name__=='__main__':main()
