// Independent Windows-side Steam Input diagnostic. This calls genuine interfaces
// through NotProton; it never manufactures controller handles or input state.
// ABI: ValveSoftware/Proton@164e0ccd2ea2b1ec2e5d08dc97f65b184c2539ca,
// lsteamclient/steamworks_sdk_153a/isteaminput.h (SteamInput006) and
// steamworks_sdk_142/{isteamclient.h,isteamutils.h} (SteamClient017/SteamUtils009).
typedef unsigned long U32;
typedef unsigned long long U64;
typedef void *Handle;
extern "C" {
int _fltused = 0;
__declspec(dllimport) Handle __stdcall LoadLibraryA(const char *);
__declspec(dllimport) Handle __stdcall LoadLibraryExA(const char *, Handle, U32);
__declspec(dllimport) void *__stdcall GetProcAddress(Handle, const char *);
__declspec(dllimport) U32 __stdcall GetEnvironmentVariableA(const char *, char *, U32);
__declspec(dllimport) Handle __stdcall GetStdHandle(U32);
__declspec(dllimport) int __stdcall WriteFile(Handle, const void *, U32, U32 *, void *);
__declspec(dllimport) void __stdcall Sleep(U32);
__declspec(dllimport) U32 __stdcall GetTickCount(void);
__declspec(dllimport) U32 __stdcall GetLastError(void);
__declspec(dllimport) __declspec(noreturn) void __stdcall ExitProcess(U32);
const char *skinos_client_identity(void *, int, int, U64 *, int *);
}

#pragma pack(push, 1)
struct DigitalData { bool state, active; };
struct AnalogData { int mode; float x, y; bool active; };
struct MotionData { float values[10]; };
#pragma pack(pop)
static_assert(sizeof(DigitalData) == 2, "Steam Input digital ABI");
static_assert(sizeof(AnalogData) == 13, "Steam Input analog ABI");
class SteamInput006 {
public:
    virtual bool Init(bool) = 0;
    virtual bool Shutdown() = 0;
    virtual bool SetInputActionManifestFilePath(const char *) = 0;
    virtual void RunFrame(bool) = 0;
    virtual bool BWaitForData(bool, U32) = 0;
    virtual bool BNewDataAvailable() = 0;
    virtual int GetConnectedControllers(U64 *) = 0;
    virtual void EnableDeviceCallbacks() = 0;
    virtual void EnableActionEventCallbacks(void *) = 0;
    virtual U64 GetActionSetHandle(const char *) = 0;
    virtual void ActivateActionSet(U64, U64) = 0;
    virtual U64 GetCurrentActionSet(U64) = 0;
    virtual void ActivateActionSetLayer(U64, U64) = 0;
    virtual void DeactivateActionSetLayer(U64, U64) = 0;
    virtual void DeactivateAllActionSetLayers(U64) = 0;
    virtual int GetActiveActionSetLayers(U64, U64 *) = 0;
    virtual U64 GetDigitalActionHandle(const char *) = 0;
    virtual DigitalData GetDigitalActionData(U64, U64) = 0;
    virtual int GetDigitalActionOrigins(U64, U64, U64, int *) = 0;
    virtual const char *GetStringForDigitalActionName(U64) = 0;
    virtual U64 GetAnalogActionHandle(const char *) = 0;
    virtual AnalogData GetAnalogActionData(U64, U64) = 0;
    virtual int GetAnalogActionOrigins(U64, U64, U64, int *) = 0;
    virtual const char *GetGlyphPNGForActionOrigin(int, int, U32) = 0;
    virtual const char *GetGlyphSVGForActionOrigin(int, U32) = 0;
    virtual const char *GetGlyphForActionOrigin_Legacy(int) = 0;
    virtual const char *GetStringForActionOrigin(int) = 0;
    virtual const char *GetStringForAnalogActionName(U64) = 0;
    virtual void StopAnalogActionMomentum(U64, U64) = 0;
    virtual MotionData GetMotionData(U64) = 0;
    virtual void TriggerVibration(U64, unsigned short, unsigned short) = 0;
    virtual void TriggerVibrationExtended(U64, unsigned short, unsigned short, unsigned short, unsigned short) = 0;
    virtual void TriggerSimpleHapticEvent(U64, int, unsigned char, char, unsigned char, char) = 0;
    virtual void SetLEDColor(U64, unsigned char, unsigned char, unsigned char, unsigned int) = 0;
    virtual void Legacy_TriggerHapticPulse(U64, int, unsigned short) = 0;
    virtual void Legacy_TriggerRepeatedHapticPulse(U64, int, unsigned short, unsigned short, unsigned short, unsigned int) = 0;
    virtual bool ShowBindingPanel(U64) = 0;
    virtual int GetInputTypeForHandle(U64) = 0;
    virtual U64 GetControllerForGamepadIndex(int) = 0;
    virtual int GetGamepadIndexForController(U64) = 0;
    virtual const char *GetStringForXboxOrigin(int) = 0;
    virtual const char *GetGlyphForXboxOrigin(int) = 0;
    virtual int GetActionOriginFromXboxOrigin(U64, int) = 0;
    virtual int TranslateActionOrigin(int, int) = 0;
    virtual bool GetDeviceBindingRevision(U64, int *, int *) = 0;
    virtual U32 GetRemotePlaySessionID(U64) = 0;
    virtual unsigned short GetSessionInputConfigurationSettings() = 0;
};
class SteamClient017 {
public:
    virtual int CreateSteamPipe() = 0;
    virtual bool BReleaseSteamPipe(int) = 0;
    virtual int ConnectToGlobalUser(int) = 0;
    virtual int CreateLocalUser(int *, int) = 0;
    virtual void ReleaseUser(int, int) = 0;
    virtual void *GetISteamUser(int, int, const char *) = 0;
    virtual void *GetISteamGameServer(int, int, const char *) = 0;
    virtual void SetLocalIPBinding(U32, unsigned short) = 0;
    virtual void *GetISteamFriends(int, int, const char *) = 0;
    virtual void *GetISteamUtils(int, const char *) = 0;
    virtual void *GetISteamMatchmaking(int, int, const char *) = 0;
    virtual void *GetISteamMatchmakingServers(int, int, const char *) = 0;
    virtual void *GetISteamGenericInterface(int, int, const char *) = 0;
};
class SteamUtils009Prefix {
public:
    virtual U32 GetSecondsSinceAppActive() = 0;
    virtual U32 GetSecondsSinceComputerActive() = 0;
    virtual int GetConnectedUniverse() = 0;
    virtual U32 GetServerRealTime() = 0;
    virtual const char *GetIPCountry() = 0;
    virtual bool GetImageSize(int, U32 *, U32 *) = 0;
    virtual bool GetImageRGBA(int, unsigned char *, int) = 0;
    virtual bool GetCSERIPPort(U32 *, unsigned short *) = 0;
    virtual unsigned char GetCurrentBatteryPower() = 0;
    virtual U32 GetAppID() = 0;
};
struct ProbeContext {
    SteamClient017 *client;
    SteamInput006 *input;
    int pipe, user;
    bool owns_pipe, sdk_initialized, input_initialized;
    void (*sdk_shutdown)(void);
    void (*sdk_callbacks)(void);
};
static ProbeContext context;
static void input_rumble_qa_cleanup();
static void cleanup() {
    input_rumble_qa_cleanup();
    if(context.input_initialized) { context.input_initialized=false;context.input->Shutdown(); }
    if(context.sdk_initialized) { context.sdk_initialized=false;context.sdk_shutdown(); }
    else if(context.owns_pipe&&context.client&&context.pipe) {
        context.owns_pipe=false;
        if(context.user)context.client->ReleaseUser(context.pipe,context.user);
        context.client->BReleaseSteamPipe(context.pipe);
    }
}
struct GamepadState {
    unsigned short buttons;
    unsigned char left_trigger, right_trigger;
    short left_x, left_y, right_x, right_y;
};
struct XInputState { U32 packet; GamepadState pad; };
static_assert(sizeof(XInputState) == 16, "XInput state ABI");
typedef U32 (__stdcall *GetXInputState)(U32, XInputState *);

static void out(const char *s) { U32 n=0,written; while(s[n]) ++n; WriteFile(GetStdHandle((U32)-11),s,n,&written,nullptr); }
static U64 decimal(const char *s) { U64 n=0; for(;*s;++s) { if(*s<'0'||*s>'9'||n>1844674407370955161ULL||(n==1844674407370955161ULL&&*s>'5')) return 0; n=n*10+*s-'0'; } return n; }
static void number(U64 n) {
    // No 64-bit integer division helper is needed in the freestanding Win32 build.
    unsigned char digits[20]={0}; char text[21]; int start=0;
    for(int bit=63;bit>=0;--bit) { unsigned carry=(unsigned)((n>>bit)&1); for(int i=19;i>=0;--i) { unsigned v=digits[i]*2+carry;digits[i]=(unsigned char)(v%10);carry=v/10; } }
    while(start<19&&!digits[start]) ++start;
    for(int i=start;i<20;++i) text[i-start]='0'+digits[i]; text[20-start]=0;out(text);
}
static void signed_number(int n) { if(n<0){out("-");number((U32)(-(n+1))+1);}else number((U32)n); }
static void boolean(bool v) { out(v?"true":"false"); }
static U32 env(const char *name,char *value,U32 capacity) { U32 n=GetEnvironmentVariableA(name,value,capacity);if(n>=capacity)return 0;return n; }
static U32 setting(const char *name,U32 fallback,U32 lower,U32 upper) { char s[32];if(!env(name,s,sizeof(s)))return fallback;U64 v=decimal(s);return v>=lower&&v<=upper?(U32)v:fallback; }
static __declspec(noreturn) void fail(const char *reason) { cleanup();out("{\"event\":\"steam_input_probe_error\",\"reason\":\"");out(reason);out("\"}\n");ExitProcess(1); }
static Handle load_steamclient(const char *path) {
    // The client is not next to this probe executable. Search the selected
    // client's directory for tier0/vstdlib as well, using the documented
    // absolute-path alternate dependency search. Relative paths are undefined
    // with LOAD_WITH_ALTERED_SEARCH_PATH and are rejected here.
    bool drive_path=((path[0]>='A'&&path[0]<='Z')||(path[0]>='a'&&path[0]<='z'))&&path[1]==':'&&path[2]=='\\';
    bool unc_path=path[0]=='\\'&&path[1]=='\\';
    if(!drive_path&&!unc_path)fail("steamclient_path_must_be_absolute");
    Handle module=LoadLibraryExA(path,nullptr,8 /* LOAD_WITH_ALTERED_SEARCH_PATH */);
    if(!module) {
        U32 error=GetLastError();
        cleanup();
        out("{\"event\":\"steam_input_probe_error\",\"reason\":\"load_steamclient_failed\",\"win32_error\":");number(error);out("}\n");ExitProcess(1);
    }
    return module;
}
static bool same_pad(const GamepadState &a,const GamepadState &b) { return a.buttons==b.buttons&&a.left_trigger==b.left_trigger&&a.right_trigger==b.right_trigger&&a.left_x==b.left_x&&a.left_y==b.left_y&&a.right_x==b.right_x&&a.right_y==b.right_y; }
// BEGIN SEMANTIC_ACTIVITY
// Diagnostic evidence thresholds only: raw SDK values/counters remain intact.
// Centres use Microsoft's XInput dead zones. Delta thresholds are conservative
// SkinOS diagnostic policy, not a game input filter or proof of physical input.
static const int activity_left_deadzone=7849,activity_right_deadzone=8689;
static const int activity_stick_delta=1024,activity_trigger_threshold=30,activity_trigger_delta=4;
struct SemanticPadTracker { GamepadState anchor;U64 handle;bool connected; };
struct SemanticAnalogTracker { AnalogData anchor;U64 handle;bool connected; };
static int activity_abs(int value) { return value<0?-value:value; }
static U32 activity_length_squared(int x,int y) { return (U32)(x*x)+(U32)(y*y); }
static bool meaningful_stick(short x,short y,short &anchor_x,short &anchor_y,int deadzone) {
    const U32 radius=(U32)(deadzone*deadzone);
    if(activity_length_squared(x,y)<=radius&&activity_length_squared(anchor_x,anchor_y)<=radius)return false;
    const int dx=(int)x-anchor_x,dy=(int)y-anchor_y;
    // Check large deltas before squaring so opposite int16 extremes cannot
    // overflow a signed multiply or the sum of two uint32 squares.
    if(activity_abs(dx)<activity_stick_delta&&activity_abs(dy)<activity_stick_delta&&
       activity_length_squared(dx,dy)<(U32)(activity_stick_delta*activity_stick_delta))return false;
    anchor_x=x;anchor_y=y;return true;
}
static bool meaningful_trigger(unsigned char value,unsigned char &anchor) {
    if(value<=activity_trigger_threshold&&anchor<=activity_trigger_threshold)return false;
    if(activity_abs((int)value-anchor)<activity_trigger_delta)return false;
    anchor=value;return true;
}
static bool observe_semantic_pad(SemanticPadTracker &tracker,bool routed,U64 handle,const GamepadState &state) {
    if(!routed||!handle) { tracker.connected=false;return false; }
    if(!tracker.connected||tracker.handle!=handle) {
        tracker.anchor=state;tracker.handle=handle;tracker.connected=true;return false;
    }
    // XInputGetState's defined button mask; reserved bits are still in raw data.
    bool changed=((state.buttons^tracker.anchor.buttons)&0xf3ff)!=0;
    tracker.anchor.buttons=state.buttons;
    // Update each control's anchor independently. Frequent button presses must
    // not erase slowly accumulated stick movement between two sample frames.
    if(meaningful_trigger(state.left_trigger,tracker.anchor.left_trigger))changed=true;
    if(meaningful_trigger(state.right_trigger,tracker.anchor.right_trigger))changed=true;
    if(meaningful_stick(state.left_x,state.left_y,tracker.anchor.left_x,tracker.anchor.left_y,activity_left_deadzone))changed=true;
    if(meaningful_stick(state.right_x,state.right_y,tracker.anchor.right_x,tracker.anchor.right_y,activity_right_deadzone))changed=true;
    return changed;
}
static bool observe_semantic_analog(SemanticAnalogTracker &tracker,U64 handle,const AnalogData &state) {
    // Pinned SteamInput006 EInputSourceMode: JoystickMove=6,
    // JoystickCamera=8, Trigger=10. Mouse-like data uses frame deltas with
    // different units; preserve it raw, but do not apply stick thresholds to it.
    bool stick=state.mode==6||state.mode==8,trigger=state.mode==10;
    bool valid=state.active&&handle&&((stick&&state.x>=-1.0f&&state.x<=1.0f&&state.y>=-1.0f&&state.y<=1.0f)||
                                    (trigger&&state.x>=0.0f&&state.x<=1.0f&&state.y==0.0f));
    // Ordered range comparisons also reject NaN and infinity.
    if(!valid) { tracker.connected=false;return false; }
    if(!tracker.connected||tracker.handle!=handle||tracker.anchor.mode!=state.mode) {
        tracker.anchor=state;tracker.handle=handle;tracker.connected=true;return false;
    }
    const float dx=state.x-tracker.anchor.x,dy=state.y-tracker.anchor.y;
    if(trigger) {
        const float centre=30.0f/255.0f,delta=4.0f/255.0f;
        if(state.x<=centre&&tracker.anchor.x<=centre)return false;
        if(dx>-delta&&dx<delta)return false;
    } else {
        const float centre=0.25f,delta=0.03125f;
        if(state.x*state.x+state.y*state.y<=centre*centre&&
           tracker.anchor.x*tracker.anchor.x+tracker.anchor.y*tracker.anchor.y<=centre*centre)return false;
        if(dx*dx+dy*dy<delta*delta)return false;
    }
    tracker.anchor=state;return true;
}
// END SEMANTIC_ACTIVITY
static void pad_json(const GamepadState &p) { out("{\"buttons\":");number(p.buttons);out(",\"left_trigger\":");number(p.left_trigger);out(",\"right_trigger\":");number(p.right_trigger);out(",\"left_x\":");signed_number(p.left_x);out(",\"left_y\":");signed_number(p.left_y);out(",\"right_x\":");signed_number(p.right_x);out(",\"right_y\":");signed_number(p.right_y);out("}"); }
static U32 float_bits(float value) { union { float f; U32 u; } v;v.f=value;return v.u; }

#include "input-rumble-qa.h"

extern "C" void mainCRTStartup(void) {
    static char client_path[2048], sdk_path[2048], expected[64], expected_app[32], manifest[2048], set_name[256], digital_name[256], analog_name[256];
    if(!env("SKINOS_EXPECTED_STEAMID",expected,sizeof(expected))||!decimal(expected))fail("missing_expected_identity");
    if(!env("SteamAppId",expected_app,sizeof(expected_app))||!decimal(expected_app)||decimal(expected_app)>0xffffffffULL)fail("missing_expected_appid");
    const U32 expected_appid=(U32)decimal(expected_app);
    if(!env("SKINOS_WINDOWS_CLIENT",client_path,sizeof(client_path))) {
#ifdef _WIN64
        const char *fallback="C:\\Program Files (x86)\\Steam\\steamclient64.dll";
#else
        const char *fallback="C:\\Program Files (x86)\\Steam\\steamclient.dll";
#endif
        unsigned i=0;do{client_path[i]=fallback[i];}while(fallback[i++]);
    }
    const char *init_method="SteamClient017";
    U32 sdk_length=GetEnvironmentVariableA("SKINOS_WINDOWS_SDK",sdk_path,sizeof(sdk_path));
    if(sdk_length>=sizeof(sdk_path))fail("invalid_sdk_path");
    if(sdk_length) {
        // A supplied game DLL is mandatory. Never replace a broken game SDK
        // with a fresh generic bridge connection that loses its app context.
        Handle sdk=LoadLibraryExA(sdk_path,nullptr,8 /* LOAD_WITH_ALTERED_SEARCH_PATH */);
        if(!sdk)fail("load_sdk_failed");
        auto init_flat=reinterpret_cast<int (*)(char *)>(GetProcAddress(sdk,"SteamAPI_InitFlat"));
        auto init_classic=reinterpret_cast<unsigned char (*)(void)>(GetProcAddress(sdk,"SteamAPI_Init"));
        context.sdk_shutdown=reinterpret_cast<void (*)(void)>(GetProcAddress(sdk,"SteamAPI_Shutdown"));
        context.sdk_callbacks=reinterpret_cast<void (*)(void)>(GetProcAddress(sdk,"SteamAPI_RunCallbacks"));
        auto get_user=reinterpret_cast<int (*)(void)>(GetProcAddress(sdk,"SteamAPI_GetHSteamUser"));
        auto get_pipe=reinterpret_cast<int (*)(void)>(GetProcAddress(sdk,"SteamAPI_GetHSteamPipe"));
        if((!init_flat&&!init_classic)||!context.sdk_shutdown)fail("sdk_init_exports_missing");
        if(!get_user||!get_pipe)fail("sdk_pipe_exports_missing");
        if(init_flat) {
            char message[1024];int result=init_flat(message);init_method="SteamAPI_InitFlat";
            if(result!=0) {
                out("{\"event\":\"steam_input_probe_error\",\"reason\":\"sdk_init_failed\",\"init_method\":\"SteamAPI_InitFlat\",\"init_result\":");signed_number(result);out("}\n");ExitProcess(2);
            }
        } else { init_method="SteamAPI_Init";if(!init_classic())fail("sdk_classic_init_failed"); }
        context.sdk_initialized=true;
        context.user=get_user();context.pipe=get_pipe();
        if(!context.user||!context.pipe)fail("sdk_connection_handles_missing");
        auto get_client=reinterpret_cast<void *(*)(void)>(GetProcAddress(sdk,"SteamClient"));
        if(get_client)context.client=static_cast<SteamClient017 *>(get_client());
    }
    if(!context.client) {
        Handle module=load_steamclient(client_path);
        auto factory=reinterpret_cast<void *(*)(const char *,int *)>(GetProcAddress(module,"CreateInterface"));
        if(!factory)fail("steamclient_factory_missing");
        context.client=static_cast<SteamClient017 *>(factory("SteamClient017",nullptr));
        if(!context.client)fail("steamclient_interface_missing");
    }
    auto client=context.client;
    if(!context.sdk_initialized) {
        context.pipe=client->CreateSteamPipe();if(!context.pipe)fail("steamclient_pipe_failed");
        context.owns_pipe=true;
        context.user=client->ConnectToGlobalUser(context.pipe);if(!context.user)fail("steamclient_user_failed");
    }
    const int pipe=context.pipe,user=context.user;
    U64 steamid=0;int online=0;
    const char *identity_error=skinos_client_identity(client,user,pipe,&steamid,&online);
    if(identity_error)fail(identity_error);
    if(!online||steamid!=decimal(expected))fail("steamclient_identity_mismatch");
    auto utils=static_cast<SteamUtils009Prefix *>(client->GetISteamUtils(pipe,"SteamUtils009"));
    if(!utils)fail("steam_utils_interface_missing");
    const U32 actual_appid=utils->GetAppID();
    const bool sdk_initialized=context.sdk_initialized;
    out("{\"event\":\"steam_input_app_context\",\"sdk_initialized\":");boolean(sdk_initialized);
    out(",\"init_method\":\"");out(init_method);out("\",\"appid\":");number(actual_appid);out(",\"expected_appid\":");number(expected_appid);
    out(",\"appid_verified\":");boolean(actual_appid==expected_appid);out(",\"pipe\":");signed_number(pipe);out(",\"user_handle\":");signed_number(user);out("}\n");
    if(actual_appid!=expected_appid)fail("steam_appid_mismatch");
    auto input=static_cast<SteamInput006 *>(client->GetISteamGenericInterface(user,pipe,"SteamInput006"));
    if(!input)fail("steam_input_interface_missing");
    if(!input->Init(true))fail("steam_input_init_failed");
    context.input=input;context.input_initialized=true;
    bool has_manifest=env("SKINOS_INPUT_ACTION_MANIFEST",manifest,sizeof(manifest))!=0;
    bool manifest_accepted=!has_manifest||input->SetInputActionManifestFilePath(manifest);
    if(!manifest_accepted)fail("action_manifest_rejected");
    bool has_set=env("SKINOS_INPUT_ACTION_SET",set_name,sizeof(set_name))!=0;
    bool has_digital=env("SKINOS_INPUT_DIGITAL_ACTION",digital_name,sizeof(digital_name))!=0;
    bool has_analog=env("SKINOS_INPUT_ANALOG_ACTION",analog_name,sizeof(analog_name))!=0;
    U64 action_set=0,digital_action=0,analog_action=0;
    Handle xinput=LoadLibraryA("xinput1_4.dll");
    if(!xinput)xinput=LoadLibraryA("xinput1_3.dll");
    auto xget=xinput?reinterpret_cast<GetXInputState>(GetProcAddress(xinput,"XInputGetState")):nullptr;
    const U32 duration=setting("SKINOS_INPUT_DURATION_MS",7000,100,60000);
    const U32 interval=setting("SKINOS_INPUT_INTERVAL_MS",100,10,1000);
    out("{\"event\":\"steam_input_probe_ready\",\"api_initialized\":true,\"identity_verified\":true,\"appid_verified\":true,\"appid\":");number(actual_appid);out(",\"sdk_initialized\":");boolean(sdk_initialized);out(",\"interface\":\"SteamInput006\",\"xinput_available\":");boolean(xget!=nullptr);out(",\"duration_ms\":");number(duration);out("}\n");
    static XInputState previous_x[4];static bool previous_connected[4];static U64 previous_mapped[4];
    static U64 previous_handles[16];static DigitalData previous_digital[16];static AnalogData previous_analog[16];
    static SemanticPadTracker semantic_x[4];static SemanticAnalogTracker semantic_analog[16];
    U32 samples=0,controller_samples=0,routed_samples=0,xinput_changes=0,action_changes=0,max_controllers=0;
    U32 xinput_meaningful_changes=0,action_meaningful_changes=0;
    U32 started=GetTickCount();
    do {
        if(context.sdk_callbacks)context.sdk_callbacks();
        input->RunFrame(true);
        if(has_set&&!action_set)action_set=input->GetActionSetHandle(set_name);
        if(has_digital&&!digital_action)digital_action=input->GetDigitalActionHandle(digital_name);
        if(has_analog&&!analog_action)analog_action=input->GetAnalogActionHandle(analog_name);
        U64 handles[16]={0};int count=input->GetConnectedControllers(handles);
        if(count<0||count>16)fail("invalid_controller_count");
        if(count)++controller_samples;if((U32)count>max_controllers)max_controllers=(U32)count;
        out("{\"event\":\"steam_input_snapshot\",\"sample\":");number(samples);out(",\"elapsed_ms\":");number(GetTickCount()-started);
        out(",\"configuration_settings\":");number(input->GetSessionInputConfigurationSettings());out(",\"controllers\":[");
        for(int i=0;i<count;++i) {
            U64 h=handles[i];if(!h)fail("invalid_zero_controller_handle");
            if(action_set)input->ActivateActionSet(h,action_set);
            int major=0,minor=0;bool revision_available=input->GetDeviceBindingRevision(h,&major,&minor);
            if(i)out(",");out("{\"handle\":\"");number(h);out("\",\"type\":");signed_number(input->GetInputTypeForHandle(h));
            out(",\"gamepad_index\":");signed_number(input->GetGamepadIndexForController(h));out(",\"action_set\":\"");number(input->GetCurrentActionSet(h));out("\",\"binding_revision_available\":");boolean(revision_available);
            out(",\"binding_major\":");if(revision_available)signed_number(major);else out("null");
            out(",\"binding_minor\":");if(revision_available)signed_number(minor);else out("null");
            out(",\"remote_play_session\":");number(input->GetRemotePlaySessionID(h));
            DigitalData digital={false,false};AnalogData analog={0,0,0,false};
            if(digital_action) { digital=input->GetDigitalActionData(h,digital_action);out(",\"digital\":{\"active\":");boolean(digital.active);out(",\"state\":");boolean(digital.state);out("}"); }
            if(analog_action) { analog=input->GetAnalogActionData(h,analog_action);out(",\"analog\":{\"active\":");boolean(analog.active);out(",\"mode\":");signed_number(analog.mode);out(",\"x_f32_bits\":");number(float_bits(analog.x));out(",\"y_f32_bits\":");number(float_bits(analog.y));out("}"); }
            if(previous_handles[i]==h) {
                if(digital.active&&previous_digital[i].active&&digital.state!=previous_digital[i].state) { ++action_changes;++action_meaningful_changes; }
                if(analog.active&&previous_analog[i].active&&(analog.x!=previous_analog[i].x||analog.y!=previous_analog[i].y))++action_changes;
            }
            if(observe_semantic_analog(semantic_analog[i],h,analog))++action_meaningful_changes;
            previous_handles[i]=h;previous_digital[i]=digital;previous_analog[i]=analog;out("}");
        }
        for(int i=count;i<16;++i) { previous_handles[i]=0;semantic_analog[i].connected=false; }
        out("],\"xinput\":[");
        for(U32 slot=0;slot<4;++slot) {
            XInputState state={};U32 result=xget?xget(slot,&state):1167;
            U64 mapped=input->GetControllerForGamepadIndex((int)slot);
            bool handle_present=false;for(int i=0;i<count;++i)if(handles[i]==mapped&&mapped)handle_present=true;
            bool routed=result==0&&handle_present&&input->GetGamepadIndexForController(mapped)==(int)slot;
            if(routed)++routed_samples;
            if(routed&&previous_connected[slot]&&previous_mapped[slot]==mapped&&!same_pad(state.pad,previous_x[slot].pad))++xinput_changes;
            bool meaningful_change=observe_semantic_pad(semantic_x[slot],routed,mapped,state.pad);
            if(meaningful_change)++xinput_meaningful_changes;
            previous_connected[slot]=routed;previous_x[slot]=state;previous_mapped[slot]=mapped;
            if(slot)out(",");out("{\"slot\":");number(slot);out(",\"result\":");number(result);out(",\"steam_input_handle\":\"");number(mapped);out("\",\"routed\":");boolean(routed);
            out(",\"meaningful_change\":");boolean(meaningful_change);
            if(result==0) { out(",\"packet\":");number(state.packet);out(",\"state\":");pad_json(state.pad); }out("}");
        }
        out("]}\n");++samples;
        if(GetTickCount()-started>=duration)break;
        Sleep(interval);
    } while(GetTickCount()-started<duration);
    run_input_rumble_qa(utils,xget,expected_appid,decimal(expected));
    bool device_seen=controller_samples!=0,activity_verified=device_seen&&(action_meaningful_changes||xinput_meaningful_changes);
    cleanup();
    out("{\"event\":\"steam_input_probe_summary\",\"scan_completed\":true,\"api_initialized\":true,\"identity_verified\":true,\"appid_verified\":true,\"appid\":");number(actual_appid);out(",\"sdk_initialized\":");boolean(sdk_initialized);out(",\"init_method\":\"");out(init_method);out("\",\"samples\":");number(samples);
    out(",\"elapsed_ms\":");number(GetTickCount()-started);out(",\"controller_seen\":");boolean(device_seen);out(",\"max_controllers\":");number(max_controllers);
    out(",\"xinput_route_observed\":");boolean(routed_samples!=0);out(",\"xinput_state_changes\":");number(xinput_changes);out(",\"action_state_changes\":");number(action_changes);
    out(",\"xinput_meaningful_changes\":");number(xinput_meaningful_changes);out(",\"action_meaningful_changes\":");number(action_meaningful_changes);
    out(",\"activity_policy\":{\"version\":1,\"left_stick_deadzone\":7849,\"right_stick_deadzone\":8689,\"stick_delta\":1024,\"trigger_threshold\":30,\"trigger_delta\":4,\"analog_action_modes\":[6,8,10],\"analog_stick_deadzone\":0.25,\"analog_stick_delta\":0.03125}");
    out(",\"steam_input_activity_verified\":");boolean(activity_verified);out(",\"physical_device_verified\":false,\"diagnostic_only\":true}\n");
    ExitProcess(0);
}
