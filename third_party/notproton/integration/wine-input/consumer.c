/* Transport conformance only; never authenticates or calls Steam APIs.
 * DirectInput public ABI declarations below are a bounded subset of Wine
 * include/dinput.h @290276fb04e812b5c4c2338ba0b8b4bd7e4bef4c (LGPL-2.1+).
 */
#define WIN32_LEAN_AND_MEAN
#include <stdarg.h>
#include "windows.h"
#include "xinput.h"
#include "protocol.h"
#include "owned-path.h"

typedef struct DeviceInstance {
    DWORD dwSize; GUID guidInstance,guidProduct; DWORD dwDevType;
    WCHAR tszInstanceName[MAX_PATH],tszProductName[MAX_PATH];
    GUID guidFFDriver; WORD wUsagePage,wUsage;
} DeviceInstance;
_Static_assert(sizeof(DeviceInstance)==1100,"DIDEVICEINSTANCEW public ABI size");
typedef BOOL (CALLBACK *EnumCallback)(const DeviceInstance*,void*);
typedef struct Input Input;
typedef struct InputVtable {
    HRESULT (WINAPI *QueryInterface)(Input*,const GUID*,void**);
    ULONG (WINAPI *AddRef)(Input*);
    ULONG (WINAPI *Release)(Input*);
    HRESULT (WINAPI *CreateDevice)(Input*,const GUID*,void**,void*);
    HRESULT (WINAPI *EnumDevices)(Input*,DWORD,EnumCallback,void*,DWORD);
} InputVtable;
struct Input { const InputVtable *v; };
typedef HRESULT (WINAPI *CreateInput)(HINSTANCE,DWORD,const GUID*,void**,void*);
typedef DWORD (WINAPI *GetState)(DWORD,XINPUT_STATE*);
static const GUID iid_input8w={0xbf798031,0x483a,0x4da2,{0xaa,0x99,0x5d,0x64,0xed,0x36,0x97,0x00}};
static DWORD dinput_total,dinput_owned;
static DWORD dinput_owned_slots;
static void out(const char*s){DWORD len=0,n;while(s[len])++len;WriteFile(GetStdHandle(STD_OUTPUT_HANDLE),s,len,&n,NULL);}
static void num(DWORD x){char b[12];unsigned n=0;do{b[n++]=(char)('0'+x%10);x/=10;}while(x);while(n){char c[2]={b[--n],0};out(c);}}
static void signed_num(int x){if(x<0){out("-");num((DWORD)-x);}else num((DWORD)x);}
static unsigned u16(const BYTE*p){return p[0]|((unsigned)p[1]<<8);}
static int s16(const BYTE*p){unsigned x=u16(p);return x&32768?(int)x-65536:(int)x;}
typedef struct DiGuidPath {DWORD size,header_size,object,how;GUID guid;WCHAR path[MAX_PATH];} DiGuidPath;
_Static_assert(sizeof(DiGuidPath)==552,"DIPROPGUIDANDPATH public ABI");
static BOOL owned_di_instance(Input *input,const DeviceInstance*instance,unsigned *slot){
    void *device=NULL;HRESULT result=input->v->CreateDevice(input,&instance->guidInstance,&device,NULL);
    if(result<0||!device)return FALSE;
    const void **v=*(const void***)device;
    HRESULT (WINAPI *get_property)(void*,const GUID*,void*)=(void*)v[5];
    ULONG (WINAPI *release)(void*)=(void*)v[2];
    DiGuidPath value;memset(&value,0,sizeof(value));value.size=sizeof(value);value.header_size=16;
    result=get_property(device,(const GUID*)12,&value);release(device);
    if(result<0)return FALSE;
    value.path[MAX_PATH-1]=0;
    return skinos_owned_ig_path((const uint16_t*)value.path,slot);
}
static BOOL CALLBACK enumerate(const DeviceInstance *device,void *unused){
    ++dinput_total;unsigned slot=0;
    if(owned_di_instance(unused,device,&slot)){++dinput_owned;dinput_owned_slots|=1u<<slot;}
    return TRUE;
}
static DWORD expected_buttons(const SkinosInputFrame*f,BOOL extended){
    DWORD b=u16(f->payload+2)&~0x0800u;
    if((b&3)==3)b&=~3u;if((b&12)==12)b&=~12u;
    if(!extended)b&=~0x0400u;return b;
}
static BOOL matches(const SkinosInputFrame*f,const XINPUT_STATE*s,BOOL extended){
    return s->Gamepad.wButtons==expected_buttons(f,extended)&&s->Gamepad.bLeftTrigger==f->payload[4]&&
        s->Gamepad.bRightTrigger==f->payload[5]&&s->Gamepad.sThumbLX==s16(f->payload+6)&&s->Gamepad.sThumbLY==s16(f->payload+8)&&
        s->Gamepad.sThumbRX==s16(f->payload+10)&&s->Gamepad.sThumbRY==s16(f->payload+12);
}
void mainCRTStartup(void){
    HMODULE ntdll=GetModuleHandleW(L"ntdll.dll");
    if(!ntdll||!GetProcAddress(ntdll,"wine_get_version")){out("{\"event\":\"wine_required\",\"ok\":false}\n");ExitProcess(1);}
    HMODULE x=LoadLibraryW(L"xinput1_4.dll"),d=LoadLibraryW(L"dinput8.dll");
    GetState get=x?(GetState)GetProcAddress(x,(const char*)100):NULL;BOOL extended=get!=NULL;
    if(!get&&x)get=(GetState)GetProcAddress(x,"XInputGetState");
    CreateInput create=d?(CreateInput)GetProcAddress(d,"DirectInput8Create"):NULL;Input*input=NULL;
    HRESULT hr=create?create(GetModuleHandleW(NULL),0x0800,&iid_input8w,(void**)&input,NULL):E_FAIL;
    if(!get||hr<0||!input){out("{\"event\":\"transport_consumer_init\",\"ok\":false}\n");ExitProcess(1);}
    out("{\"event\":\"transport_consumer_ready\",\"transport_only\":true,\"steam_input_verified\":false,\"xinput_extended\":");out(extended?"true":"false");out("}\n");
    DWORD failure=0;
    for(;;){
        SkinosInputFrame f;DWORD have=0;
        while(have<sizeof(f)){DWORD count=0;if(!ReadFile(GetStdHandle(STD_INPUT_HANDLE),(BYTE*)&f+have,sizeof(f)-have,&count,NULL)||!count)break;have+=count;}
        if(!have)break;
        if(have!=sizeof(f)||f.magic!=SKINOS_INPUT_MAGIC||f.version!=1||f.slot>=4||
           !((f.kind==2&&f.payload_len==20)||(f.kind==3&&f.payload_len==0))){failure=1;out("{\"event\":\"invalid_expectation\",\"ok\":false}\n");break;}
        DWORD started=GetTickCount(),code=ERROR_DEVICE_NOT_CONNECTED,connected=0;BOOL ok=FALSE;XINPUT_STATE actual={0};
        do{
            code=get(f.slot,&actual);dinput_total=dinput_owned=dinput_owned_slots=0;
            connected=0;for(DWORD slot=0;slot<4;slot++){XINPUT_STATE check;if(get(slot,&check)==0)++connected;}
            hr=input->v->EnumDevices(input,4,enumerate,input,1); /* GAMECTRL, ATTACHEDONLY */
            /* This consumer proves only an otherwise-empty single-pad fixture.
             * A matching foreign/global-first pad is never sufficient. */
            ok=hr>=0&&(f.kind==3?(connected==0&&dinput_total==0):
                (!code&&connected==1&&matches(&f,&actual,extended)&&dinput_total==1&&dinput_owned==1&&dinput_owned_slots==(1u<<f.slot)));
            if(ok)break;Sleep(25);
        }while(GetTickCount()-started<5000);
        out("{\"event\":\"transport_comparison\",\"transport_only\":true,\"steam_input_verified\":false,\"ok\":");out(ok?"true":"false");
        out(",\"slot\":");num(f.slot);out(",\"kind\":");num(f.kind);out(",\"xinput_result\":");num(code);
        out(",\"dinput_result\":");num((DWORD)hr);out(",\"dinput_devices\":");num(dinput_total);out(",\"dinput_owned_devices\":");num(dinput_owned);
        out(",\"xinput_connected_count\":");num(connected);out(",\"isolated_single_pad\":true");
        out(",\"dinput_owned_slots\":");num(dinput_owned_slots);out(",\"dinput_identity_source\":\"DIPROP_GUIDANDPATH\"");
        out(",\"buttons\":");num(actual.Gamepad.wButtons);out(",\"expected_buttons\":");num(expected_buttons(&f,extended));
        out(",\"lt\":");num(actual.Gamepad.bLeftTrigger);out(",\"rt\":");num(actual.Gamepad.bRightTrigger);
        out(",\"lx\":");signed_num(actual.Gamepad.sThumbLX);out(",\"ly\":");signed_num(actual.Gamepad.sThumbLY);
        out(",\"rx\":");signed_num(actual.Gamepad.sThumbRX);out(",\"ry\":");signed_num(actual.Gamepad.sThumbRY);out("}\n");
        if(!ok)failure=1;
    }
    input->v->Release(input);ExitProcess(failure);
}
