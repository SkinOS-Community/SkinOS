/* Read-only reconnect observer, separate from the strict conformance consumer.
 * Only public XInput/DirectInput APIs. The one-byte stdin command requests a
 * snapshot; it never supplies state or reaches a producer/device-control API. */
#define mainCRTStartup original_transport_entry
#include "consumer.c"
#undef mainCRTStartup
#include "dinput-state.h"

struct observed_device { GUID guid; WCHAR path[MAX_PATH]; unsigned slot,generation; };
static struct observed_device observed_devices[16];
static unsigned observed_count;
static GUID acquired_guid;
static WCHAR acquired_path[MAX_PATH];

static void hex_digit(unsigned digit){char s[2]={"0123456789abcdef"[digit&15],0};out(s);}
static void hex_number(DWORD value,unsigned digits){while(digits){--digits;hex_digit(value>>(digits*4));}}
static void guid_json(const GUID *guid){
    out("\"");hex_number(guid->Data1,8);out("-");hex_number(guid->Data2,4);out("-");hex_number(guid->Data3,4);out("-");
    for(unsigned i=0;i<8;i++){if(i==2)out("-");hex_number(guid->Data4[i],2);}out("\"");
}
static void path_json(const WCHAR *path){
    out("\"");for(;*path;path++){
        if(*path=='\\'||*path=='\"'){char s[3]={'\\',(char)*path,0};out(s);}
        else if(*path>=32&&*path<127){char s[2]={(char)*path,0};out(s);}
        else {out("\\u");hex_number(*path,4);}
    }out("\"");
}
static BOOL query_owned_path(Input *input,const DeviceInstance *instance,DiGuidPath *path,unsigned *slot){
    void *device=NULL;HRESULT result=input->v->CreateDevice(input,&instance->guidInstance,&device,NULL);
    if(result<0||!device)return FALSE;
    const void **v=*(const void***)device;
    HRESULT (WINAPI *get_property)(void*,const GUID*,void*)=(void*)v[5];
    ULONG (WINAPI *release)(void*)=(void*)v[2];
    memset(path,0,sizeof(*path));path->size=sizeof(*path);path->header_size=16;
    result=get_property(device,(const GUID*)12,path);release(device);
    if(result<0)return FALSE;
    path->path[MAX_PATH-1]=0;
    return skinos_owned_ig_path((const uint16_t*)path->path,slot);
}
static unsigned path_generation(const WCHAR *path){
    /* Full path was already validated by skinos_owned_ig_path. The last '-'
     * before the GUID separator follows our owned slot and precedes 8 hex digits. */
    const WCHAR *p=path,*serial=NULL;
    while(*p){if(*p=='#'){const uint16_t *test=(const uint16_t *)(p+1);
        if(skinos_path_literal(&test,"skinos-wine-output-")){serial=(const WCHAR *)test;break;}}++p;}
    if(!serial)return 0;
    p=serial+3;unsigned generation=0;
    for(unsigned i=0;i<8;i++){unsigned c=skinos_path_lower((uint16_t)p[i]);
        generation=(generation<<4)|(c<='9'?c-'0':c-'a'+10);}
    return generation;
}
static BOOL CALLBACK enumerate_observer(const DeviceInstance *instance,void *context){
    Input *input=context;unsigned slot=0;DiGuidPath path;
    enumerate_di_state(instance,context);
    if(observed_count<16&&query_owned_path(input,instance,&path,&slot)){
        struct observed_device *item=&observed_devices[observed_count++];
        item->guid=instance->guidInstance;memcpy(item->path,path.path,sizeof(item->path));
        item->slot=slot;item->generation=path_generation(path.path);
    }
    return TRUE;
}
static void emit_xinput(GetState get){
    out("[");for(DWORD slot=0;slot<4;slot++){
        XINPUT_STATE state={0};DWORD result=get(slot,&state);if(slot)out(",");
        out("{\"slot\":");num(slot);out(",\"result\":");num(result);
        if(!result){out(",\"packet\":");num(state.dwPacketNumber);out(",\"state\":{\"buttons\":");num(state.Gamepad.wButtons);
            out(",\"left_trigger\":");num(state.Gamepad.bLeftTrigger);out(",\"right_trigger\":");num(state.Gamepad.bRightTrigger);
            out(",\"left_x\":");signed_num(state.Gamepad.sThumbLX);out(",\"left_y\":");signed_num(state.Gamepad.sThumbLY);
            out(",\"right_x\":");signed_num(state.Gamepad.sThumbRX);out(",\"right_y\":");signed_num(state.Gamepad.sThumbRY);out("}");}
        out("}");
    }out("]");
}
void mainCRTStartup(void){
    HMODULE ntdll=GetModuleHandleW(L"ntdll.dll");
    if(!ntdll||!GetProcAddress(ntdll,"wine_get_version")){out("{\"event\":\"input_observer_error\",\"ok\":false,\"reason\":\"wine_required\"}\n");ExitProcess(1);}
    HMODULE x=LoadLibraryW(L"xinput1_4.dll"),d=LoadLibraryW(L"dinput8.dll");
    GetState get=x?(GetState)GetProcAddress(x,"XInputGetState"):NULL;
    CreateInput create=d?(CreateInput)GetProcAddress(d,"DirectInput8Create"):NULL;Input *input=NULL;
    HRESULT result=create?create(GetModuleHandleW(NULL),0x0800,&iid_input8w,(void**)&input,NULL):E_FAIL;
    if(!get||result<0||!input){out("{\"event\":\"input_observer_error\",\"ok\":false,\"reason\":\"public_api_init\"}\n");ExitProcess(1);}
    out("{\"event\":\"input_observer_ready\",\"schema_version\":1,\"read_only\":true,\"physical_device_verified\":false}\n");
    BYTE request;DWORD count=0,failure=0;
    while(ReadFile(GetStdHandle(STD_INPUT_HANDLE),&request,1,&count,NULL)&&count){
        if(request!=1){failure=1;break;}
        dinput_total=dinput_owned=dinput_owned_slots=0;di_instance_found=FALSE;observed_count=0;
        result=input->v->EnumDevices(input,4,enumerate_observer,input,1);
        HRESULT state_result=E_FAIL;SkinosDiState state;
        if(di_device&&(observed_count!=1||memcmp(&acquired_guid,&observed_devices[0].guid,sizeof(GUID))||
            memcmp(acquired_path,observed_devices[0].path,sizeof(acquired_path))))close_di_state();
        if(result>=0&&observed_count==1&&dinput_owned==1){
            di_instance=observed_devices[0].guid;acquired_guid=di_instance;
            memcpy(acquired_path,observed_devices[0].path,sizeof(acquired_path));
            state_result=read_di_state(input,&state);
        }
        out("{\"event\":\"input_observer_snapshot\",\"schema_version\":1,\"read_only\":true,\"physical_device_verified\":false,\"windows_pid\":");num(GetCurrentProcessId());
        out(",\"dinput_enum_result\":");num((DWORD)result);out(",\"dinput_total\":");num(dinput_total);
        out(",\"dinput_owned\":");num(dinput_owned);out(",\"owned\":[");
        for(unsigned i=0;i<observed_count;i++){struct observed_device *item=&observed_devices[i];if(i)out(",");
            out("{\"slot\":");num(item->slot);out(",\"generation\":");num(item->generation);out(",\"guid\":");guid_json(&item->guid);
            out(",\"path\":");path_json(item->path);out("}");}
        out("],\"dinput_state_result\":");num((DWORD)state_result);out(",\"dinput_state\":");
        if(state_result>=0)emit_di_state(&state);else out("null");out(",\"xinput\":");emit_xinput(get);out("}\n");
    }
    close_di_state();input->v->Release(input);ExitProcess(failure);
}
