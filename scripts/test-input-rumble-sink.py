#!/usr/bin/env python3
"""Actual retained sink header policy tests with SDK/Win32 mocks, never Wine."""
import hashlib
import importlib.util
import json
import pathlib
import subprocess

ROOT=pathlib.Path(__file__).resolve().parents[1]
OUT=ROOT/'target/input-rumble-sink-experiment/tests'
HEADER=ROOT/'third_party/notproton/integration/input-rumble-sink.h'
CODE=r'''
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <map>
#include <stdexcept>
#include <string>
#include <tuple>
#include <vector>
#include <thread>
#include <atomic>
#include <mutex>
#include <condition_variable>
#include <chrono>
#define __stdcall
#define __declspec(x) __attribute__((x))
#define SKINOS_SINK_TEST
using U32=uint32_t;using U64=uint64_t;using Handle=void*;
struct GamepadState {unsigned short buttons;unsigned char left_trigger,right_trigger;short left_x,left_y,right_x,right_y;};
struct XInputState {U32 packet;GamepadState pad;};
using GetXInputState=U32 (*)(U32,XInputState*);
static U32 ticks,last_error,read_failure,xinput_delay;
static bool reader_auto_progress=true;
static std::atomic<bool> block_read{false},read_entered{false},release_read{false};
static std::mutex read_mutex;static std::condition_variable read_condition;static bool identity_ok=true,writer_progress=true,reader_eof=true;
static std::string output;static std::vector<unsigned char> incoming;static size_t incoming_at;static U32 max_read=64;
static std::map<std::string,std::string> environment;
struct Exit:std::runtime_error {U32 code;Exit(U32 c,const char*s):runtime_error(s),code(c){}};
struct SteamUtils009Prefix {U32 appid=42;U32 GetAppID(){return appid;}};
struct FakeInput {
 int count=1,slot=0;bool reciprocal=true;U64 handle=55;unsigned shutdowns=0,pumps=0,reveal_after_pumps=0,lose_identity_after_pumps=0;
 std::vector<std::tuple<U64,unsigned short,unsigned short>> calls;
 int GetConnectedControllers(U64 *h){if(count>0)h[0]=handle;return count;}
 int GetGamepadIndexForController(U64){return slot;}
 U64 GetControllerForGamepadIndex(int){return reciprocal?handle:99;}
 void TriggerVibration(U64 h,unsigned short l,unsigned short r){calls.emplace_back(h,l,r);}
 void RunFrame(bool){++pumps;if(reveal_after_pumps&&pumps>=reveal_after_pumps)count=1;if(lose_identity_after_pumps&&pumps>=lose_identity_after_pumps)identity_ok=false;}
 bool Shutdown(){++shutdowns;return true;}
};
static unsigned sdk_shutdowns;static void sdk_shutdown(){++sdk_shutdowns;}
static unsigned sdk_callback_pumps;static void pump_callbacks(){++sdk_callback_pumps;}
struct Context {bool input_initialized=true,sdk_initialized=true;FakeInput *input;void *client;int user=1,pipe=1;void(*sdk_callbacks)()=nullptr;void(*sdk_shutdown)()=::sdk_shutdown;};
static Context context;
static const char *skinos_client_identity(void*,int,int,U64 *id,int *online){*id=123;*online=identity_ok;return nullptr;}
static U32 GetEnvironmentVariableA(const char *name,char *value,U32 capacity){auto it=environment.find(name);if(it==environment.end())return 0;auto n=it->second.size();if(n>=capacity)return n+1;std::memcpy(value,it->second.c_str(),n+1);return n;}
static Handle GetStdHandle(U32 n){return reinterpret_cast<Handle>(uintptr_t(n));}
static U32 GetTickCount(){return ticks;}static U32 GetLastError(){return last_error;}
static void Sleep(U32);
static int WriteFile(Handle,const void *bytes,U32 length,U32 *written,void*){output.append(static_cast<const char*>(bytes),length);*written=length;return 1;}
static int ReadFile(Handle,void *bytes,U32 length,U32 *got,void*){
 if(block_read){read_entered=true;std::unique_lock<std::mutex> lock(read_mutex);read_condition.wait(lock,[]{return release_read.load();});*got=0;return 1;}
 if(read_failure){last_error=read_failure;*got=0;return 0;}
 *got=std::min({length,max_read,U32(incoming.size()-incoming_at)});std::memcpy(bytes,incoming.data()+incoming_at,*got);incoming_at+=*got;return 1;
}
static Handle CreateThread(void*,size_t,U32 (*)(void*),void*,U32,U32*){return reinterpret_cast<Handle>(1);}
static int CloseHandle(Handle){return 1;}
[[noreturn]]static void ExitProcess(U32 code){throw Exit(code,"ExitProcess");}
static void input_rumble_sink_cleanup();
[[noreturn]]static void fail(const char *why){input_rumble_sink_cleanup();throw Exit(1,why);}
static U32 get_state(U32 slot,XInputState *state){if(!slot){ticks+=xinput_delay;xinput_delay=0;}*state={};state->pad.buttons=0x1000;return slot==0?0:1167;}
static Handle LoadLibraryA(const char*){return reinterpret_cast<Handle>(1);}
static void *GetProcAddress(Handle,const char*){return reinterpret_cast<void*>(get_state);}
#include "input-rumble-sink.h"
// Simulate only writer scheduling; SDK methods are never called here.
static void Sleep(U32 value){ticks+=value;
 if(reader_auto_progress&&!sink_atomic(&sink_input.stop)&&!sink_atomic(&sink_input.done)&&(incoming_at<incoming.size()||reader_eof||read_failure)){
  if(!sink_read_once())sink_store(&sink_input.done,1);
 }
 if(writer_progress){while(sink_output.count){U32 i=sink_output.head;output.append(sink_output.lines[i],sink_output.lengths[i]);sink_output.head=(i+1)%sink_queue_capacity;--sink_output.count;}if(sink_output.stop)sink_output.done=1;}}

#define CHECK(x) do{if(!(x)){std::cerr<<"FAIL "<<__LINE__<<" "<<#x<<"\n";std::exit(1);}}while(0)
static FakeInput input;static SteamUtils009Prefix utils;
static void reset(){input={};utils={};context={};context.input=&input;context.client=&input;ticks=0;last_error=0;read_failure=0;xinput_delay=0;reader_auto_progress=true;block_read=false;read_entered=false;release_read=false;identity_ok=true;writer_progress=true;reader_eof=true;output.clear();incoming.clear();incoming_at=0;max_read=64;sdk_shutdowns=0;sink_output={};sink_input={};sink_reader_handle=nullptr;input_sink={};input_sink.utils=&utils;input_sink.xget=get_state;input_sink.appid=42;input_sink.steamid=123;environment={{"SKINOS_INPUT_FEEDBACK_SINK","1"}};}
static SkinosSinkFrame frame(U32 kind,U64 seq,U32 gen=3,U32 left=0,U32 right=0){SkinosSinkFrame f={};f.magic=sink_magic;f.version=1;f.bytes=64;f.kind=kind;f.epoch_low=77;f.control_session=12;f.slot=0;f.generation=gen;f.sequence_low=U32(seq);f.sequence_high=U32(seq>>32);f.left=left;f.right=right;return f;}
static const char *accept(SkinosSinkFrame f,U32 now=0){bool shutdown=false;return sink_accept(f,now,&shutdown);}
static void append(SkinosSinkFrame f){auto *p=reinterpret_cast<unsigned char*>(&f);incoming.insert(incoming.end(),p,p+sizeof(f));}
static U32 run(){try{run_input_rumble_sink(&utils,get_state,42,123);return 777;}catch(const Exit&e){return e.code;}}
int main(){unsigned tests=0;
 reset();CHECK(!accept(frame(1,1)));CHECK(!accept(frame(3,2)));input.count=0;input.handle=88;input.reveal_after_pumps=3;sdk_callback_pumps=0;context.sdk_callbacks=pump_callbacks;
 CHECK(!accept(frame(1,3,4)));CHECK(input_sink.bound&&input_sink.handle==88&&input_sink.generation==4&&ticks==30&&input.pumps==3&&sdk_callback_pumps==3&&input.calls.empty());Sleep(0);
 CHECK(output.find("\"attempts\":4,\"wait_ms\":30")!=std::string::npos);++tests;
 reset();input.count=0;CHECK(std::string(accept(frame(1,1)))=="bind_route_unverified");CHECK(ticks==500&&input.pumps==50&&!input_sink.bound&&input.calls.empty());Sleep(0);
 CHECK(output.find("\"attempts\":50,\"wait_ms\":500")!=std::string::npos);++tests;
 reset();input.count=0;input.lose_identity_after_pumps=1;CHECK(std::string(accept(frame(1,1)))=="bind_route_unverified");CHECK(ticks==10&&input.pumps==1&&!input_sink.bound&&input.calls.empty());++tests;
 reset();input.count=2;CHECK(std::string(accept(frame(1,1)))=="bind_route_unverified");CHECK(ticks==0&&input.pumps==0&&!input_sink.bound&&input.calls.empty());++tests;
 reset();environment.clear();CHECK(run()==777&&input.calls.empty());++tests;
 reset();environment["SKINOS_INPUT_FEEDBACK_SINK"]="true";CHECK(run()==1&&input.calls.empty());++tests;
 reset();context.sdk_initialized=false;CHECK(run()==1&&input.calls.empty());++tests;
 reset();xinput_delay=1881;append(frame(1,1));append(frame(5,2));CHECK(run()==0);CHECK(input.calls.empty());CHECK(input_sink.began==1881);
 CHECK(output.find("\"xinput_warmup_ms\":1881")!=std::string::npos&&output.find("\"xinput_warmup_calls\":4")!=std::string::npos);
 CHECK(output.find("\"xinput_ms\":0")!=std::string::npos&&output.find("\"total_ms\":0")!=std::string::npos);++tests;
 reset();xinput_delay=850;CHECK(!accept(frame(1,1)));Sleep(0);CHECK(output.find("\"xinput_ms\":850")!=std::string::npos&&output.find("\"total_ms\":850")!=std::string::npos);CHECK(output.find("\"xinput_calls\":4")!=std::string::npos&&input.calls.empty());++tests;
 reset();CHECK(!accept(frame(1,1)));CHECK(input_sink.handle==55);CHECK(input.calls.empty());CHECK(!accept(frame(2,2,3,1000,2000)));CHECK(input.calls.back()==std::make_tuple(U64(55),(unsigned short)1000,(unsigned short)2000));CHECK(!sink_tick(249)&&input.calls.size()==1);CHECK(!sink_tick(250)&&input.calls.size()==2);CHECK(!accept(frame(4,3),1000));CHECK(!sink_tick(2000)&&input.calls.size()==3);CHECK(sink_tick(2500)!=nullptr);CHECK(sink_stop_old());CHECK(input.calls.back()==std::make_tuple(U64(55),(unsigned short)0,(unsigned short)0));++tests;
 for(int which=0;which<16;++which){reset();CHECK(!accept(frame(1,1)));auto f=frame(2,2,3,1000,2000);switch(which){case 0:f.magic=0;break;case 1:f.version=2;break;case 2:f.bytes=63;break;case 3:f.kind=6;break;case 4:f.reserved0=1;break;case 5:f.reserved1=1;break;case 6:f.reserved2=1;break;case 7:f.sequence_low=1;break;case 8:f.sequence_low=3;break;case 9:f.epoch_low=78;break;case 10:f.control_session=13;break;case 11:f.generation=2;break;case 12:f.slot=1;break;case 13:f.left=65536;break;case 14:f.right=65536;break;case 15:f.kind=4;break;}CHECK(accept(f)!=nullptr);CHECK(input.calls.empty());++tests;}
 for(int which=0;which<5;++which){reset();CHECK(!accept(frame(1,1)));switch(which){case 0:identity_ok=false;break;case 1:utils.appid=43;break;case 2:input.handle=99;break;case 3:input.count=2;break;case 4:input.reciprocal=false;break;}const char *result=accept(frame(2,2,3,1,2));CHECK((which<2?result!=nullptr:result==nullptr)&&input.calls.empty());if(which>=2)CHECK(input_sink.suspended);++tests;}
 reset();CHECK(!accept(frame(1,1)));CHECK(!accept(frame(2,2,3,1,2)));input.handle=99;CHECK(!sink_tick(10));CHECK(input_sink.suspended&&!input_sink.left&&!input_sink.right);CHECK(!sink_stop_old());CHECK(input.calls.size()==1);CHECK(!accept(frame(2,3,3,65535,65535),20));CHECK(input.calls.size()==1);CHECK(!accept(frame(4,4),1000));CHECK(std::string(sink_tick(1510))=="route_suspended_timeout");CHECK(!accept(frame(3,5),1511));CHECK(!input_sink.bound&&!input_sink.suspended);CHECK(!accept(frame(1,6,4),1512));CHECK(input_sink.handle==99);CHECK(!accept(frame(2,7,4,3,4),1513));CHECK(input.calls.size()==2&&std::get<0>(input.calls.back())==99);++tests;
 reset();CHECK(!accept(frame(1,1)));CHECK(!accept(frame(2,2,3,1,2)));CHECK(!accept(frame(3,3)));CHECK(input.calls.back()==std::make_tuple(U64(55),(unsigned short)0,(unsigned short)0));CHECK(!input_sink.bound&&!input_sink.left&&!input_sink.right);CHECK(!accept(frame(4,4,0),1000));CHECK(!sink_tick(1000));CHECK(accept(frame(1,5,3))!=nullptr);input.handle=88;CHECK(!accept(frame(1,5,4),1000));CHECK(input_sink.handle==88);CHECK(!accept(frame(2,6,4,7,9),1000));CHECK(std::get<0>(input.calls.back())==88);++tests;
 reset();for(unsigned i=0;i<sink_queue_capacity;++i){SinkText text;text.add("bounded\n");CHECK(sink_enqueue(text));}SinkText text;text.add("overflow\n");CHECK(!sink_enqueue(text));CHECK(input.calls.empty());++tests;
 reset();CHECK(!accept(frame(1,1)));CHECK(!accept(frame(2,2,3,11,22)));writer_progress=false;CHECK(input_sink.stop_pending);try{sink_close(reinterpret_cast<Handle>(1),false,"blocked_writer");}catch(const Exit&e){CHECK(e.code==1);}CHECK(ticks==200&&sdk_shutdowns==1&&input.shutdowns==1);CHECK(std::get<1>(input.calls.back())==0);++tests;
 reset();append(frame(1,1));append(frame(2,2,3,4000,5000));append(frame(5,3));max_read=7;CHECK(run()==0);CHECK(input.calls.size()==2&&std::get<1>(input.calls.back())==0);CHECK(output.find("\"reason\":\"shutdown\"")!=std::string::npos&&output.find("\"sdk_dispatched\":true")!=std::string::npos);CHECK(sdk_shutdowns==1&&input.shutdowns==1);++tests;
 reset();append(frame(1,1));append(frame(2,2,3,4000,5000));CHECK(run()==0);CHECK(input.calls.size()==2&&std::get<1>(input.calls.back())==0);CHECK(output.find("\"reason\":\"eof\"")!=std::string::npos);++tests;
 reset();append(frame(1,1));append(frame(2,2,3,4000,5000));reader_eof=false;CHECK(run()==1);CHECK(ticks>=sink_lease_ms&&ticks<=sink_lease_ms+200);CHECK(std::get<1>(input.calls.back())==0);CHECK(output.find("heartbeat_expired")!=std::string::npos);++tests;
 reset();reader_eof=false;CHECK(run()==1&&input.calls.empty());CHECK(ticks>=sink_start_ms&&ticks<=sink_start_ms+200);CHECK(output.find("initial_bind_timeout")!=std::string::npos);++tests;
 reset();read_failure=6;CHECK(run()==1&&input.calls.empty());CHECK(output.find("stdin_read_failed")!=std::string::npos&&output.find("\"transport_win32_error\":6")!=std::string::npos);++tests;
 reset();input.count=0;append(frame(1,1));CHECK(run()==1&&input.calls.empty());CHECK(output.find("bind_route_unverified")!=std::string::npos&&output.find("\"transport_win32_error\":0")!=std::string::npos);++tests;
 reset();incoming.push_back(1);CHECK(run()==1);CHECK(input.calls.empty()&&output.find("truncated_control_eof")!=std::string::npos);++tests;
 reset();incoming.push_back(1);reader_eof=false;CHECK(run()==1);CHECK(ticks>=250&&ticks<=450&&input.calls.empty());CHECK(output.find("partial_control_timeout")!=std::string::npos);++tests;
 reset();incoming.resize(64*65);while(sink_read_once()){}CHECK(sink_input.error==2);CHECK(run()==1&&input.calls.empty());CHECK(output.find("control_queue_flood")!=std::string::npos);++tests;
 reset();CHECK(!accept(frame(1,1)));CHECK(!accept(frame(2,2,3,1000,2000)));reader_auto_progress=false;block_read=true;
 std::thread blocked([]{sink_reader(nullptr);});
 auto deadline=std::chrono::steady_clock::now()+std::chrono::seconds(2);while(!read_entered){CHECK(std::chrono::steady_clock::now()<deadline);std::this_thread::yield();}
 CHECK(std::string(sink_tick(sink_lease_ms))=="heartbeat_expired");
 try{sink_close(reinterpret_cast<Handle>(1),false,"heartbeat_expired");}catch(const Exit&e){CHECK(e.code==1);}
 CHECK(sdk_shutdowns==1&&input.shutdowns==1&&input.calls.size()==2&&std::get<1>(input.calls.back())==0);CHECK(ticks<=200);
 release_read=true;read_condition.notify_one();blocked.join();CHECK(sink_input.done==1);++tests;
 reset();SinkText first;first.add("first\n");SinkText second;second.add("second\n");CHECK(sink_enqueue(first)&&sink_enqueue(second));sink_output.stop=1;CHECK(sink_writer(nullptr)==0);CHECK(output=="first\nsecond\n"&&input.calls.empty());++tests;
 std::cout<<"OFFLINE ONLY: "<<tests<<" retained SDK sink header cases passed; Win32/SDK mocks, no real API/Wine calls.\n";
}
'''

def main():
    OUT.mkdir(parents=True,exist_ok=True)
    assert 'PeekNamedPipe' not in HEADER.read_text()
    source=OUT/'sink-header-mock.cpp'
    source.write_text('#include <algorithm>\n'+CODE)
    executable=OUT/'sink-header-mock'
    command=['clang++','-std=c++17','-O1','-g','-Wall','-Wextra','-Werror','-fsanitize=address,undefined','-fno-omit-frame-pointer','-I',str(HEADER.parent),str(source),'-o',str(executable)]
    subprocess.run(command,check=True)
    result=subprocess.run([str(executable)],capture_output=True,text=True,check=True)
    spec=importlib.util.spec_from_file_location('sink_builder',ROOT/'scripts/build-input-rumble-sink.py')
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    original=(HEADER.parent/'input-probe.cpp').read_text()
    transformed=module.transform(original)
    assert transformed.count('    run_input_rumble_sink(')==1
    assert transformed.index('    run_input_rumble_sink(') < transformed.index('input->SetInputActionManifestFilePath')
    for boundary in ['static ProbeContext context;','#include "input-rumble-qa.h"','    context.input=input;context.input_initialized=true;']:
        try: module.transform(original.replace(boundary,''))
        except RuntimeError: pass
        else: raise AssertionError('changed anchor accepted')
    report={'offline_only':True,'header_sha256':hashlib.sha256(HEADER.read_bytes()).hexdigest(),'stdout':result.stdout,'stderr':result.stderr,'compiler_command':command,'transform_anchor_guards':True}
    (OUT/'report.json').write_text(json.dumps(report,indent=2)+'\n')
    print(result.stdout,end='')

if __name__=='__main__':main()
