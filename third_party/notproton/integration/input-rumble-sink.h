// Isolated retained genuine SteamInput006 sink. Not included by production.
// All SDK calls stay on the main thread. The writer thread performs only I/O.
#ifndef SKINOS_INPUT_RUMBLE_SINK_H
#define SKINOS_INPUT_RUMBLE_SINK_H
#ifndef SKINOS_SINK_TEST
extern "C" {
__declspec(dllimport) int __stdcall ReadFile(Handle,void*,U32,U32*,void*);
__declspec(dllimport) Handle __stdcall CreateThread(void*,__SIZE_TYPE__,U32 (__stdcall *)(void*),void*,U32,U32*);
__declspec(dllimport) int __stdcall CloseHandle(Handle);
}
#endif
// Exactly sixteen LE u32 fields. Windows x86/x64 are little endian.
// Every command uses the next connection sequence (including heartbeat).
// Bind=1, Submit=2, Retire=3, Ping=4, Shutdown=5; unused motors/reserved must be0.
struct SkinosSinkFrame {
    U32 magic,version,bytes,kind;
    U32 epoch_low,epoch_high,control_session,slot;
    U32 generation,sequence_low,sequence_high,left;
    U32 right,reserved0,reserved1,reserved2;
};
static_assert(sizeof(SkinosSinkFrame)==64,"retained sink control frame");
static const U32 sink_magic=0x53495253u,sink_version=1;
static const U32 sink_lease_ms=1500,sink_refresh_ms=250,sink_start_ms=5000,sink_bind_wait_ms=500;
static const U32 sink_queue_capacity=32,sink_line_capacity=512;
struct SinkOutputQueue {
    U32 lock,head,count,error,win32_error,stop,done;
    U32 lengths[sink_queue_capacity];
    char lines[sink_queue_capacity][sink_line_capacity];
};
static SinkOutputQueue sink_output;
static bool sink_lock() {
    for(unsigned i=0;i<1024;++i) {
        U32 expected=0;
        if(__atomic_compare_exchange_n(&sink_output.lock,&expected,1,false,__ATOMIC_ACQUIRE,__ATOMIC_RELAXED))return true;
    }
    return false;
}
static void sink_unlock(){__atomic_store_n(&sink_output.lock,0,__ATOMIC_RELEASE);}
static U32 sink_atomic(const U32 *value){return __atomic_load_n(value,__ATOMIC_ACQUIRE);}
static void sink_store(U32 *value,U32 number){__atomic_store_n(value,number,__ATOMIC_RELEASE);}
struct SinkText {
    char bytes[sink_line_capacity];U32 length;bool valid;
    SinkText():length(0),valid(true){}
    void add(const char *s){for(;*s;++s){if(length==sink_line_capacity){valid=false;return;}bytes[length++]=*s;}}
    void number(U64 value){
        unsigned char digits[20]={0};int start=0;
        for(int bit=63;bit>=0;--bit){unsigned carry=(unsigned)((value>>bit)&1);for(int i=19;i>=0;--i){unsigned v=digits[i]*2+carry;digits[i]=(unsigned char)(v%10);carry=v/10;}}
        while(start<19&&!digits[start])++start;
        for(int i=start;i<20;++i){char text[2]={(char)('0'+digits[i]),0};add(text);}
    }
};
static bool sink_enqueue(const SinkText &text) {
    if(!text.valid||!text.length||sink_atomic(&sink_output.error)||!sink_lock())return false;
    bool accepted=sink_output.count<sink_queue_capacity;
    if(accepted){U32 at=(sink_output.head+sink_output.count)%sink_queue_capacity;sink_output.lengths[at]=text.length;
        for(U32 i=0;i<text.length;++i)sink_output.lines[at][i]=text.bytes[i];++sink_output.count;}
    sink_unlock();return accepted;
}
static U32 __stdcall sink_writer(void*) {
    for(;;){
        char bytes[sink_line_capacity];U32 length=0;
        if(!sink_lock()){Sleep(1);continue;}
        if(sink_output.count){length=sink_output.lengths[sink_output.head];for(U32 i=0;i<length;++i)bytes[i]=sink_output.lines[sink_output.head][i];}
        sink_unlock();
        if(!length){if(sink_atomic(&sink_output.stop))break;Sleep(1);continue;}
        U32 position=0;
        while(position<length){U32 written=0;int ok=WriteFile(GetStdHandle((U32)-11),bytes+position,length-position,&written,nullptr);if(!ok||!written||written>length-position){sink_store(&sink_output.win32_error,ok?0:GetLastError());sink_store(&sink_output.error,1);sink_store(&sink_output.done,1);return 1;}position+=written;}
        while(!sink_lock())Sleep(1);
        sink_output.head=(sink_output.head+1)%sink_queue_capacity;--sink_output.count;sink_unlock();
    }
    sink_store(&sink_output.done,1);return 0;
}
// Blocking stdin belongs exclusively to this I/O worker, never the SDK owner.
// Data queued before EOF must be consumed before the EOF boundary is observed.
static const U32 sink_input_capacity=4096;
struct SinkInputQueue {
    U32 lock,head,count,error,win32_error,closed,stop,done;
    unsigned char bytes[sink_input_capacity];
};
static SinkInputQueue sink_input;
static Handle sink_reader_handle;
static bool sink_input_lock(){
    for(unsigned i=0;i<1024;++i){U32 expected=0;if(__atomic_compare_exchange_n(&sink_input.lock,&expected,1,false,__ATOMIC_ACQUIRE,__ATOMIC_RELAXED))return true;}
    return false;
}
static void sink_input_unlock(){__atomic_store_n(&sink_input.lock,0,__ATOMIC_RELEASE);}
// One blocking OS read; only sink_reader (or its offline I/O mock) calls this.
static bool sink_read_once(){
    unsigned char bytes[256];U32 got=0;
    int ok=ReadFile(GetStdHandle((U32)-10),bytes,sizeof(bytes),&got,nullptr);
    if(!ok||!got||got>sizeof(bytes)){
        U32 error=ok?0:GetLastError();
        if((ok&&!got)||error==109||error==232||error==233)sink_store(&sink_input.closed,1);
        else {sink_store(&sink_input.win32_error,error);sink_store(&sink_input.error,1);}
        return false;
    }
    while(!sink_input_lock()){if(sink_atomic(&sink_input.stop))return false;Sleep(1);}
    if(got>sink_input_capacity-sink_input.count){sink_input_unlock();sink_store(&sink_input.error,2);return false;}
    U32 at=(sink_input.head+sink_input.count)%sink_input_capacity;
    for(U32 i=0;i<got;++i)sink_input.bytes[(at+i)%sink_input_capacity]=bytes[i];
    sink_input.count+=got;sink_input_unlock();return true;
}
static U32 __stdcall sink_reader(void*){
    while(!sink_atomic(&sink_input.stop)&&sink_read_once()){}
    sink_store(&sink_input.done,1);return sink_atomic(&sink_input.error)?1:0;
}
// 0=temporarily empty/contended, 1=data, 2=drained EOF, 3=I/O error, 4=flood.
static U32 sink_take_input(unsigned char *bytes,U32 capacity,U32 *got){
    *got=0;U32 error=sink_atomic(&sink_input.error);if(error)return error==2?4:3;
    if(!sink_input_lock())return 0;
    U32 take=capacity<sink_input.count?capacity:sink_input.count;
    for(U32 i=0;i<take;++i)bytes[i]=sink_input.bytes[(sink_input.head+i)%sink_input_capacity];
    sink_input.head=(sink_input.head+take)%sink_input_capacity;sink_input.count-=take;
    U32 closed=sink_atomic(&sink_input.closed);sink_input_unlock();
    *got=take;if(take)return 1;
    // Snapshot EOF under the same lock as the empty queue. Otherwise a reader
    // can enqueue its final bytes and publish EOF between unlock and this test.
    return closed?2:0;
}
struct SinkState {
    SteamUtils009Prefix *utils;GetXInputState xget;
    U64 steamid,epoch,sequence,desired_sequence,handle;
    U32 appid,control_session,generation,highest_generation,slot,last_ping,last_refresh,began,transport_error,suspended_at;
    unsigned short left,right;
    bool bound,stop_pending,suspended;
};
static SinkState input_sink;
static bool sink_identity() {
    auto &s=input_sink;U64 id=0;int online=0;
    return context.sdk_initialized&&context.input_initialized&&s.utils&&s.utils->GetAppID()==s.appid
        &&!skinos_client_identity(context.client,context.user,context.pipe,&id,&online)&&online&&id==s.steamid;
}
struct SinkRouteTiming {U32 identity_ms,controllers_ms,mapping_ms,xinput_ms,total_ms,last_stage,xinput_calls,attempts,wait_ms;int controller_count;bool retryable;};
static bool sink_route_result(SinkRouteTiming *timing,U32 began,bool result){if(timing)timing->total_ms=(U32)(GetTickCount()-began);return result;}
static bool sink_route(U64 *actual,SinkRouteTiming *timing=nullptr) {
    U32 began=timing?GetTickCount():0,stage=began;
    bool identity=sink_identity();
    if(timing){timing->identity_ms=(U32)(GetTickCount()-stage);timing->last_stage=1;stage=GetTickCount();}
    if(!identity)return sink_route_result(timing,began,false);
    U64 handles[16]={0};int count=context.input->GetConnectedControllers(handles);
    if(timing){timing->controllers_ms=(U32)(GetTickCount()-stage);timing->controller_count=count;timing->last_stage=2;stage=GetTickCount();}
    if(count!=1||!handles[0]){if(timing)timing->retryable=count==0;return sink_route_result(timing,began,false);}
    const auto &s=input_sink;
    if(s.bound&&handles[0]!=s.handle)return sink_route_result(timing,began,false);
    bool mapped=context.input->GetGamepadIndexForController(handles[0])==(int)s.slot&&context.input->GetControllerForGamepadIndex((int)s.slot)==handles[0];
    if(timing){timing->mapping_ms=(U32)(GetTickCount()-stage);timing->last_stage=3;stage=GetTickCount();}
    if(!mapped){if(timing)timing->retryable=true;return sink_route_result(timing,began,false);}
    U32 connected=0;
    for(U32 slot=0;slot<4;++slot){XInputState state={};U32 result=s.xget?s.xget(slot,&state):1167;
        if(timing){timing->xinput_calls+=s.xget?1:0;timing->xinput_ms=(U32)(GetTickCount()-stage);timing->last_stage=4;}
        if(result==0){++connected;if(slot!=s.slot)return sink_route_result(timing,began,false);}else if(result!=1167)return sink_route_result(timing,began,false);}
    if(connected!=1){if(timing)timing->retryable=connected==0;return sink_route_result(timing,began,false);}
    *actual=handles[0];if(timing)timing->last_stage=5;return sink_route_result(timing,began,true);
}
// A fresh parent binding may precede the SDK's hotplug callback. Only a
// temporarily absent/mapping-incomplete route can settle here. Identity loss,
// multiple controllers, an unexpected XInput error or another live slot fail
// immediately. All calls remain on the SDK thread, with no motor dispatch.
static bool sink_bind_route(U64 *actual,SinkRouteTiming *timing){
    U32 began=GetTickCount();bool verified=false;
    for(;;){
        if(timing->attempts&&(U32)(GetTickCount()-began)>=sink_bind_wait_ms)break;
        SinkRouteTiming attempt={};verified=sink_route(actual,&attempt);++timing->attempts;
        timing->identity_ms+=attempt.identity_ms;timing->controllers_ms+=attempt.controllers_ms;
        timing->mapping_ms+=attempt.mapping_ms;timing->xinput_ms+=attempt.xinput_ms;
        timing->xinput_calls+=attempt.xinput_calls;timing->last_stage=attempt.last_stage;timing->controller_count=attempt.controller_count;
        if(verified||!attempt.retryable||sink_atomic(&sink_output.error))break;
        U32 elapsed=(U32)(GetTickCount()-began);if(elapsed>=sink_bind_wait_ms)break;
        U32 wait_began=GetTickCount();
        if(context.sdk_callbacks)context.sdk_callbacks();context.input->RunFrame(true);
        elapsed=(U32)(GetTickCount()-began);
        if(elapsed<sink_bind_wait_ms){U32 remaining=sink_bind_wait_ms-elapsed;Sleep(remaining<10?remaining:10);}
        timing->wait_ms+=(U32)(GetTickCount()-wait_began);
    }
    timing->total_ms=(U32)(GetTickCount()-began);return verified;
}
static bool sink_bind_timing(const SkinosSinkFrame &f,const SinkRouteTiming &timing,bool verified){
    SinkText text;text.add("{\"event\":\"steam_input_sink_bind_timing\",\"epoch\":\"");text.number(((U64)f.epoch_high<<32)|f.epoch_low);
    text.add("\",\"control_session\":");text.number(f.control_session);text.add(",\"slot\":");text.number(f.slot);
    text.add(",\"generation\":");text.number(f.generation);text.add(",\"sequence\":\"");text.number(((U64)f.sequence_high<<32)|f.sequence_low);
    text.add("\",\"identity_ms\":");text.number(timing.identity_ms);text.add(",\"controllers_ms\":");text.number(timing.controllers_ms);
    text.add(",\"mapping_ms\":");text.number(timing.mapping_ms);text.add(",\"xinput_ms\":");text.number(timing.xinput_ms);
    text.add(",\"total_ms\":");text.number(timing.total_ms);text.add(",\"last_stage\":");text.number(timing.last_stage);
    text.add(",\"controller_count\":");if(timing.controller_count<0){text.add("-");text.number((U32)(-(timing.controller_count+1))+1);}else text.number((U32)timing.controller_count);
    text.add(",\"xinput_calls\":");text.number(timing.xinput_calls);text.add(",\"route_verified\":");text.add(verified?"true":"false");
    text.add(",\"attempts\":");text.number(timing.attempts);text.add(",\"wait_ms\":");text.number(timing.wait_ms);
    text.add(",\"physical_device_verified\":false}\n");return sink_enqueue(text);
}

static bool sink_stop_old() {
    auto &s=input_sink;bool pending=s.stop_pending;s.stop_pending=false;s.left=0;s.right=0;s.desired_sequence=0;
    if(!pending||!s.handle||!sink_identity())return false;
    U64 handles[16]={0};int count=context.input->GetConnectedControllers(handles);
    if(count<0||count>16)return false;
    for(int i=0;i<count;++i)if(handles[i]==s.handle){context.input->TriggerVibration(s.handle,0,0);return true;}
    return false;
}
static void input_rumble_sink_cleanup(){(void)sink_stop_old();}
static bool sink_ack(U32 kind,bool dispatched,const char *event="steam_input_sink_ack") {
    auto &s=input_sink;SinkText text;text.add("{\"event\":\"");text.add(event);text.add("\",\"kind\":");text.number(kind);
    text.add(",\"epoch\":\"");text.number(s.epoch);text.add("\",\"control_session\":");text.number(s.control_session);
    text.add(",\"slot\":");text.number(s.slot);text.add(",\"generation\":");text.number(s.generation);
    text.add(",\"sequence\":\"");text.number(s.sequence);text.add("\",\"desired_sequence\":\"");text.number(s.desired_sequence);text.add("\",\"steam_input_handle\":\"");text.number(s.handle);
    text.add("\",\"left\":");text.number(s.left);text.add(",\"right\":");text.number(s.right);
    text.add(",\"sdk_dispatched\":");text.add(dispatched?"true":"false");text.add(",\"physical_device_verified\":false}\n");
    return sink_enqueue(text);
}
static bool sink_suspend(U32 now){
    if(input_sink.suspended)return true;
    bool stopped=sink_stop_old();input_sink.suspended=true;input_sink.suspended_at=now;
    return sink_ack(0,stopped,"steam_input_sink_suspended");
}
static bool sink_rejected(const SkinosSinkFrame &f){
    auto &s=input_sink;SinkText text;text.add("{\"event\":\"steam_input_sink_rejected\",\"kind\":2,\"reason\":\"route_suspended\",\"epoch\":\"");text.number(s.epoch);
    text.add("\",\"control_session\":");text.number(s.control_session);text.add(",\"slot\":");text.number(s.slot);text.add(",\"generation\":");text.number(s.generation);
    text.add(",\"sequence\":\"");text.number(s.sequence);text.add("\",\"steam_input_handle\":\"");text.number(s.handle);
    text.add("\",\"requested_left\":");text.number(f.left);text.add(",\"requested_right\":");text.number(f.right);
    text.add(",\"desired_sequence\":\"0\",\"left\":0,\"right\":0,\"sdk_dispatched\":false,\"physical_device_verified\":false}\n");
    return sink_enqueue(text);
}
static U64 sink_u64(U32 low,U32 high){return ((U64)high<<32)|low;}
// Returns null on accepted command, or a fixed non-secret error code. Rejected
// commands never send their requested motors; caller cleanup may stop old ones.
static const char *sink_accept(const SkinosSinkFrame &f,U32 now,bool *shutdown) {
    auto &s=input_sink;*shutdown=false;
    U64 epoch=sink_u64(f.epoch_low,f.epoch_high),seq=sink_u64(f.sequence_low,f.sequence_high);
    if(f.magic!=sink_magic||f.version!=sink_version||f.bytes!=sizeof(f)||f.kind<1||f.kind>5||f.reserved0||f.reserved1||f.reserved2)return "malformed_control";
    if(!epoch||!seq||s.sequence==~(U64)0||seq!=s.sequence+1||f.slot!=0||f.left>65535||f.right>65535)return "invalid_control_identity_or_sequence";
    if(f.kind!=2&&(f.left||f.right))return "motors_outside_submit";
    if(s.epoch&&(epoch!=s.epoch||f.control_session!=s.control_session))return "wrong_control_connection";
    if(!f.control_session)return "missing_control_session";
    if(f.kind==1){
        if(s.bound||!f.generation||f.generation<=s.highest_generation)return "binding_not_fresh";
        U64 actual=0;SinkRouteTiming timing={};bool route=sink_bind_route(&actual,&timing);
        if(!sink_bind_timing(f,timing,route))return "output_queue_unavailable";
        if(!route)return "bind_route_unverified";
        s.epoch=epoch;s.control_session=f.control_session;s.generation=f.generation;s.highest_generation=f.generation;
        s.slot=f.slot;s.handle=actual;s.bound=true;s.suspended=false;s.sequence=seq;s.last_ping=now+timing.total_ms;s.last_refresh=now+timing.total_ms;
        return sink_ack(f.kind,false)?nullptr:"output_queue_unavailable";
    }
    if(!s.epoch||f.generation!=(s.bound?s.generation:0))return "wrong_binding_generation";
    if(f.kind==2){
        if(!s.bound)return "submit_without_binding";
        if(!sink_identity())return "identity_lost";
        U64 actual=0;
        if(s.suspended||!sink_route(&actual)){
            if(!sink_suspend(now))return "output_queue_unavailable";
            s.sequence=seq;return sink_rejected(f)?nullptr:"output_queue_unavailable";
        }
        s.sequence=seq;s.desired_sequence=seq;s.left=(unsigned short)f.left;s.right=(unsigned short)f.right;
        s.stop_pending=(s.left||s.right);s.last_refresh=now;
        context.input->TriggerVibration(s.handle,s.left,s.right);
        return sink_ack(f.kind,true)?nullptr:"output_queue_unavailable";
    }
    if(f.kind==3){
        s.sequence=seq;
        if(!s.bound)return "retire_without_binding";
        bool stopped=sink_stop_old();bool ack=sink_ack(f.kind,stopped);
        s.bound=false;s.handle=0;s.generation=0;s.suspended=false;
        return ack?nullptr:"output_queue_unavailable";
    }
    if(f.kind==4){
        if(!sink_identity())return "heartbeat_identity_lost";
        if(s.bound&&!s.suspended){U64 actual=0;if(!sink_route(&actual)&&!sink_suspend(now))return "output_queue_unavailable";}
        s.sequence=seq;s.last_ping=now;return sink_ack(f.kind,false)?nullptr:"output_queue_unavailable";
    }
    s.sequence=seq;*shutdown=true;return nullptr;
}
static const char *sink_tick(U32 now) {
    auto &s=input_sink;
    if(sink_atomic(&sink_output.error))return "stdout_writer_failed";
    if(!s.epoch){if((U32)(now-s.began)>=sink_start_ms)return "initial_bind_timeout";return nullptr;}
    if((U32)(now-s.last_ping)>=sink_lease_ms)return "heartbeat_expired";
    if(!sink_identity())return "identity_lost";
    if(s.bound&&!s.suspended){U64 actual=0;if(!sink_route(&actual)&&!sink_suspend(now))return "output_queue_unavailable";}
    if(s.suspended&&(U32)(now-s.suspended_at)>=sink_lease_ms)return "route_suspended_timeout";
    if(s.bound&&!s.suspended&&(s.left||s.right)&&(U32)(now-s.last_refresh)>=sink_refresh_ms){
        context.input->TriggerVibration(s.handle,s.left,s.right);s.last_refresh=now;
        if(!sink_ack(2,true,"steam_input_sink_refresh"))return "output_queue_unavailable";
    }
    return nullptr;
}
static __declspec(noreturn) void sink_close(Handle writer,bool success,const char *reason) {
    sink_store(&sink_input.stop,1);
    bool zero=sink_stop_old();bool input_shutdown=true,sdk_shutdown=false;
    if(context.input_initialized){context.input_initialized=false;input_shutdown=context.input->Shutdown();}
    if(context.sdk_initialized){context.sdk_initialized=false;context.sdk_shutdown();sdk_shutdown=true;}
    SinkText text;text.add("{\"event\":\"steam_input_sink_stopped\",\"reason\":\"");text.add(reason);
    text.add("\",\"zero_dispatched\":");text.add(zero?"true":"false");text.add(",\"input_shutdown\":");text.add(input_shutdown?"true":"false");
    text.add(",\"sdk_shutdown_dispatched\":");text.add(sdk_shutdown?"true":"false");
    text.add(",\"epoch\":\"");text.number(input_sink.epoch);text.add("\",\"control_session\":");text.number(input_sink.control_session);
    text.add(",\"generation\":");text.number(input_sink.generation);text.add(",\"sequence\":\"");text.number(input_sink.sequence);
    text.add("\",\"transport_win32_error\":");text.number(input_sink.transport_error?input_sink.transport_error:sink_atomic(&sink_output.win32_error));
    text.add(",\"physical_device_verified\":false}\n");
    bool queued=sink_enqueue(text);sink_store(&sink_output.stop,1);
    U32 began=GetTickCount();while(!sink_atomic(&sink_output.done)&&(U32)(GetTickCount()-began)<200)Sleep(5);
    bool drained=sink_atomic(&sink_output.done)&&!sink_atomic(&sink_output.error);
    if(writer)CloseHandle(writer);
    // Closing a thread handle does not cancel ReadFile. Do not wait for that
    // I/O worker: ExitProcess ends it inside this owned helper after SDK stop.
    if(sink_reader_handle){CloseHandle(sink_reader_handle);sink_reader_handle=nullptr;}
    ExitProcess(success&&input_shutdown&&sdk_shutdown&&queued&&drained?0:1);
}
static void run_input_rumble_sink(SteamUtils009Prefix *utils,GetXInputState xget,U32 appid,U64 steamid) {
    char flag[8];U32 n=GetEnvironmentVariableA("SKINOS_INPUT_FEEDBACK_SINK",flag,sizeof(flag));
    if(!n)return;
    if(n!=1||flag[0]!='1')fail("invalid_feedback_sink_opt_in");
    if(!context.sdk_initialized||!context.input_initialized)fail("feedback_sink_requires_real_game_sdk");
    if(!xget){
        Handle module=LoadLibraryA("xinput1_4.dll");if(!module)module=LoadLibraryA("xinput1_3.dll");
        if(module)xget=reinterpret_cast<GetXInputState>(GetProcAddress(module,"XInputGetState"));
    }
    if(!xget)fail("feedback_sink_xinput_unavailable");
    input_sink.utils=utils;input_sink.xget=xget;input_sink.appid=appid;input_sink.steamid=steamid;
    if(!sink_identity())fail("feedback_sink_identity_unverified");
    // Wine lazily enumerates HID devices on the first real XInput read. Do this
    // read-only work before advertising readiness; Bind still verifies the full
    // current identity and reciprocal route without any relaxed guard.
    U32 warmup_began=GetTickCount(),warmup_calls=0;
    for(U32 slot=0;slot<4;++slot){XInputState state={};xget(slot,&state);++warmup_calls;}
    U32 warmup_ms=(U32)(GetTickCount()-warmup_began);
    if(!sink_identity())fail("feedback_sink_identity_unverified");
    input_sink.began=GetTickCount();
    Handle writer=CreateThread(nullptr,0,sink_writer,nullptr,0,nullptr);
    if(!writer)fail("feedback_sink_writer_unavailable");
    SinkText ready;ready.add("{\"event\":\"steam_input_sink_ready\",\"sdk_initialized\":true,\"identity_verified\":true,\"appid_verified\":true,\"appid\":");ready.number(appid);
    ready.add(",\"xinput_warmup_ms\":");ready.number(warmup_ms);ready.add(",\"xinput_warmup_calls\":");ready.number(warmup_calls);
    ready.add(",\"control_frame_bytes\":64,\"version\":1,\"refresh_ms\":250,\"heartbeat_lease_ms\":1500,\"physical_device_verified\":false}\n");
    if(!sink_enqueue(ready))sink_close(writer,false,"output_queue_unavailable");
    sink_reader_handle=CreateThread(nullptr,0,sink_reader,nullptr,0,nullptr);
    if(!sink_reader_handle){input_sink.transport_error=GetLastError();sink_close(writer,false,"stdin_read_failed");}
    unsigned char partial[64];U32 filled=0,partial_since=0,rate_since=GetTickCount(),rate_count=0;
    for(;;){
        if(context.sdk_callbacks)context.sdk_callbacks();context.input->RunFrame(true);
        U32 now=GetTickCount();if(const char *error=sink_tick(now))sink_close(writer,false,error);
        for(U32 budget=0;budget<8;++budget){
            U32 got=0;U32 input=sink_take_input(partial+filled,64-filled,&got);
            if(input==0)break;
            if(input==2)sink_close(writer,filled==0,filled?"truncated_control_eof":"eof");
            if(input==3){input_sink.transport_error=sink_atomic(&sink_input.win32_error);sink_close(writer,false,"stdin_read_failed");}
            if(input==4)sink_close(writer,false,"control_queue_flood");
            if(!filled)partial_since=now;filled+=got;
            if(filled==64){
                SkinosSinkFrame frame;unsigned char *destination=(unsigned char*)&frame;for(U32 i=0;i<64;++i)destination[i]=partial[i];filled=0;
                if((U32)(now-rate_since)>=1000){rate_since=now;rate_count=0;}
                if(++rate_count>1024)sink_close(writer,false,"control_rate_flood");
                bool shutdown=false;if(const char *error=sink_accept(frame,now,&shutdown))sink_close(writer,false,error);
                if(shutdown)sink_close(writer,true,"shutdown");
            }
        }
        if(filled&&(U32)(now-partial_since)>=250)sink_close(writer,false,"partial_control_timeout");
        Sleep(5);
    }
}
#endif
