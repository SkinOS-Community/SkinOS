#!/usr/bin/env python3
"""Compile/run offline SDK-helper policy mocks; no Wine, Steam, or production build."""
import hashlib
import json
import pathlib
import subprocess

ROOT=pathlib.Path(__file__).resolve().parents[1]
OUT=ROOT/'target/input-rumble-qa/tests'
HEADER=ROOT/'third_party/notproton/integration/input-rumble-qa.h'
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
using U32=uint32_t;using U64=uint64_t;
struct GamepadState {unsigned short buttons;unsigned char left_trigger,right_trigger;short left_x,left_y,right_x,right_y;};
struct XInputState {U32 packet;GamepadState pad;};
using GetXInputState=U32 (*)(U32,XInputState*);
static U32 ticks;static std::string output;static bool identity_ok,extra_xinput,neutral,changed_handle,xinput_error;
static std::map<std::string,std::string> environment;
struct SteamUtils009Prefix {U32 appid=648800;U32 GetAppID(){return appid;}};
struct FakeInput {
 int count=1,slot=0;bool reciprocal=true;std::vector<std::tuple<U64,unsigned short,unsigned short>> calls;
 U64 current(){return changed_handle&&!calls.empty()?99:55;}
 int GetConnectedControllers(U64 *h){if(count>0)h[0]=current();return count;}
 int GetGamepadIndexForController(U64){return slot;}
 U64 GetControllerForGamepadIndex(int){return reciprocal?current():77;}
 void TriggerVibration(U64 h,unsigned short l,unsigned short r){calls.emplace_back(h,l,r);}
 void RunFrame(bool){}
};
struct Context {bool input_initialized=true,sdk_initialized=true;FakeInput *input;void *client;int user=1,pipe=1;void(*sdk_callbacks)()=nullptr;};
static Context context;
static const char *skinos_client_identity(void*,int,int,U64 *id,int *online){*id=123;*online=identity_ok;return nullptr;}
static U32 GetEnvironmentVariableA(const char *name,char *value,U32 capacity){auto it=environment.find(name);if(it==environment.end())return 0;auto n=it->second.size();if(n>=capacity)return n+1;std::memcpy(value,it->second.c_str(),n+1);return n;}
static U32 env(const char *name,char *value,U32 capacity){U32 n=GetEnvironmentVariableA(name,value,capacity);return n>=capacity?0:n;}
static U64 decimal(const char *s){U64 n=0;for(;*s;++s){if(*s<'0'||*s>'9')return 0;n=n*10+*s-'0';}return n;}
static void out(const char *s){output+=s;}static void number(U64 n){output+=std::to_string(n);}
static void signed_number(int n){output+=std::to_string(n);}static void boolean(bool b){output+=b?"true":"false";}
static U32 GetTickCount(){return ticks;}static void Sleep(U32 value){ticks+=value;}
static void input_rumble_qa_cleanup();
[[noreturn]]static void fail(const char *why){input_rumble_qa_cleanup();throw std::runtime_error(why);}
static U32 get_state(U32 slot,XInputState *state){*state={};if(xinput_error&&slot==2)return 5;state->pad.left_y=neutral?1:20000;return (slot==0||(extra_xinput&&slot==1))?0:1167;}
#include "input-rumble-qa.h"
#define CHECK(x) do{if(!(x)){std::cerr<<"FAIL "<<__LINE__<<" "<<#x<<"\n";std::exit(1);}}while(0)
static void reset(FakeInput &input,SteamUtils009Prefix &utils){input={};utils={};context={};context.input=&input;context.client=&input;ticks=0;output.clear();identity_ok=true;extra_xinput=false;neutral=true;changed_handle=false;xinput_error=false;rumble_qa_target={};environment={{"SKINOS_INPUT_RUMBLE_TEST","1"},{"SKINOS_STEAM_INPUT_FIXTURE","neutral"},{"SKINOS_INPUT_RUMBLE_FIXTURE_ID","77"}};}
static bool run(FakeInput &input,SteamUtils009Prefix &utils){(void)input;try{run_input_rumble_qa(&utils,get_state,648800,123);return true;}catch(const std::runtime_error&){return false;}}
int main(){FakeInput input;SteamUtils009Prefix utils;unsigned tests=0;
 reset(input,utils);environment.clear();CHECK(run(input,utils));CHECK(input.calls.empty());++tests;
 reset(input,utils);CHECK(run(input,utils));CHECK(input.calls.size()==4);CHECK(input.calls[0]==std::make_tuple(U64(55),(unsigned short)0x4000,(unsigned short)0));CHECK(input.calls[1]==std::make_tuple(U64(55),(unsigned short)0,(unsigned short)0));CHECK(input.calls[2]==std::make_tuple(U64(55),(unsigned short)0,(unsigned short)0x6000));CHECK(input.calls[3]==input.calls[1]);CHECK(ticks==2000);CHECK(output.find("\"callback_verified\":true")==std::string::npos);CHECK(!rumble_qa_target.stop_pending);++tests;
 for(int which=0;which<9;++which){reset(input,utils);switch(which){case 0:environment["SKINOS_INPUT_RUMBLE_TEST"]="0";break;case 1:environment.erase("SKINOS_STEAM_INPUT_FIXTURE");break;case 2:environment["SKINOS_INPUT_RUMBLE_FIXTURE_ID"]="0";break;case 3:context.sdk_initialized=false;break;case 4:identity_ok=false;break;case 5:utils.appid=999;break;case 6:input.count=2;break;case 7:input.reciprocal=false;break;case 8:extra_xinput=true;break;}CHECK(!run(input,utils));CHECK(input.calls.empty());++tests;}
 reset(input,utils);xinput_error=true;CHECK(!run(input,utils));CHECK(input.calls.empty());++tests;
 reset(input,utils);neutral=false;CHECK(!run(input,utils));CHECK(input.calls.empty());++tests;
 reset(input,utils);changed_handle=true;CHECK(!run(input,utils));CHECK(input.calls.size()==1);CHECK(output.find("\"zero_dispatched\":false")!=std::string::npos);for(auto &c:input.calls)CHECK(std::get<0>(c)!=99);++tests;
 reset(input,utils);rumble_qa_target={55,123,648800,77,0,&utils,get_state,true};input_rumble_qa_cleanup();CHECK(input.calls.size()==1);CHECK(input.calls[0]==std::make_tuple(U64(55),(unsigned short)0,(unsigned short)0));input_rumble_qa_cleanup();CHECK(input.calls.size()==1);++tests;
 std::cout<<"OFFLINE ONLY: "<<tests<<" actual QA-header policy cases passed; mocks, no SDK/Wine calls.\n";
}
'''

def main():
    OUT.mkdir(parents=True,exist_ok=True)
    source=OUT/'sdk-header-mock.cpp';source.write_text(CODE)
    executable=OUT/'sdk-header-mock'
    command=['clang++','-std=c++17','-O1','-g','-Wall','-Wextra','-Werror','-fsanitize=address,undefined',
             '-fno-omit-frame-pointer','-I',str(HEADER.parent),str(source),'-o',str(executable)]
    subprocess.run(command,check=True)
    result=subprocess.run([str(executable)],capture_output=True,text=True,check=True)
    report={'offline_only':True,'header_sha256':hashlib.sha256(HEADER.read_bytes()).hexdigest(),
            'stdout':result.stdout,'stderr':result.stderr,'compiler_command':command}
    (OUT/'report.json').write_text(json.dumps(report,indent=2)+'\n')
    print(result.stdout,end='')

if __name__=='__main__':main()
