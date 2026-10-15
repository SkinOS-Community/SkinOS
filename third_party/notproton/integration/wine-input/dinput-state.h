#ifndef SKINOS_DINPUT_STATE_H
#define SKINOS_DINPUT_STATE_H
/* Public DirectInput8 C ABI subset, from Wine include/dinput.h at the pinned
 * commit. LGPL-2.1+, see LICENSE.Wine. No private engine/Wine vtable is used. */
#include "dinput-expect.h"
typedef struct DiDevice DiDevice;
typedef struct DiObjectFormat {const GUID *guid;DWORD offset,type,flags;} DiObjectFormat;
typedef struct DiFormat {DWORD size,object_size,flags,data_size,count;DiObjectFormat *objects;} DiFormat;
typedef struct DiPropertyHeader {DWORD size,header_size,object,how;} DiPropertyHeader;
typedef struct DiPropertyRange {DiPropertyHeader header;LONG min,max;} DiPropertyRange;
typedef struct DiPropertyNumber {DiPropertyHeader header;DWORD value;} DiPropertyNumber;
typedef struct DiDeviceVtable {
    HRESULT (WINAPI *QueryInterface)(DiDevice*,const GUID*,void**);
    ULONG (WINAPI *AddRef)(DiDevice*);
    ULONG (WINAPI *Release)(DiDevice*);
    void *GetCapabilities,*EnumObjects;
    HRESULT (WINAPI *GetProperty)(DiDevice*,const GUID*,DiPropertyHeader*);
    HRESULT (WINAPI *SetProperty)(DiDevice*,const GUID*,const DiPropertyHeader*);
    HRESULT (WINAPI *Acquire)(DiDevice*);
    HRESULT (WINAPI *Unacquire)(DiDevice*);
    HRESULT (WINAPI *GetDeviceState)(DiDevice*,DWORD,void*);
    void *GetDeviceData;
    HRESULT (WINAPI *SetDataFormat)(DiDevice*,const DiFormat*);
    void *SetEventNotification;
    HRESULT (WINAPI *SetCooperativeLevel)(DiDevice*,HWND,DWORD);
    void *GetObjectInfo,*GetDeviceInfo,*RunControlPanel,*Initialize,*CreateEffect,*EnumEffects,*GetEffectInfo;
    void *GetForceFeedbackState,*SendForceFeedbackCommand,*EnumCreatedEffectObjects,*Escape;
    HRESULT (WINAPI *Poll)(DiDevice*);
} DiDeviceVtable;
struct DiDevice {const DiDeviceVtable *v;};
_Static_assert(offsetof(DiDeviceVtable,GetDeviceState)==9*sizeof(void*),"DI public state slot");
_Static_assert(offsetof(DiDeviceVtable,Poll)==25*sizeof(void*),"DI public poll slot");
_Static_assert(sizeof(DiPropertyRange)==24,"DIPROPRANGE public layout");
static const GUID di_axes[5]={
    {0xa36d02e0,0xc9f3,0x11cf,{0xbf,0xc7,0x44,0x45,0x53,0x54,0,0}},
    {0xa36d02e1,0xc9f3,0x11cf,{0xbf,0xc7,0x44,0x45,0x53,0x54,0,0}},
    {0xa36d02e2,0xc9f3,0x11cf,{0xbf,0xc7,0x44,0x45,0x53,0x54,0,0}},
    {0xa36d02f4,0xc9f3,0x11cf,{0xbf,0xc7,0x44,0x45,0x53,0x54,0,0}},
    {0xa36d02f5,0xc9f3,0x11cf,{0xbf,0xc7,0x44,0x45,0x53,0x54,0,0}}
};
static const GUID di_pov={0xa36d02f2,0xc9f3,0x11cf,{0xbf,0xc7,0x44,0x45,0x53,0x54,0,0}};
static const GUID di_button={0xa36d02f0,0xc9f3,0x11cf,{0xbf,0xc7,0x44,0x45,0x53,0x54,0,0}};
static DiDevice *di_device;
static GUID di_instance;
static BOOL di_instance_found;
static BOOL CALLBACK enumerate_di_state(const DeviceInstance*device,void*unused){
    ++dinput_total;unsigned slot=0;
    if(owned_di_instance(unused,device,&slot)){
        ++dinput_owned;dinput_owned_slots|=1u<<slot;di_instance=device->guidInstance;di_instance_found=TRUE;
    }
    return TRUE;
}
static HRESULT open_di_state(Input*input){
    HRESULT result=input->v->CreateDevice(input,&di_instance,(void**)&di_device,NULL);
    if(result<0)return result;
    DiObjectFormat objects[16];
    for(unsigned i=0;i<5;i++){objects[i].guid=&di_axes[i];objects[i].offset=i*4;objects[i].type=0x00ffff02;objects[i].flags=0;}
    objects[5].guid=&di_pov;objects[5].offset=20;objects[5].type=0x00ffff10;objects[5].flags=0;
    for(unsigned i=0;i<10;i++){objects[6+i].guid=&di_button;objects[6+i].offset=24+i;objects[6+i].type=4|(i<<8);objects[6+i].flags=0;}
    DiFormat format={sizeof(format),sizeof(objects[0]),1,sizeof(SkinosDiState),16,objects};
    if((result=di_device->v->SetDataFormat(di_device,&format))<0)goto fail;
    HMODULE user=GetModuleHandleW(L"user32.dll");
    HWND (WINAPI *desktop)(void)=user?(void*)GetProcAddress(user,"GetDesktopWindow"):NULL;
    if(!desktop){result=E_FAIL;goto fail;}
    if((result=di_device->v->SetCooperativeLevel(di_device,desktop(),2|8))<0)goto fail;
    for(unsigned i=0;i<5;i++){
        DiPropertyRange range={{sizeof(range),sizeof(range.header),i*4,1},0,65535};
        if((result=di_device->v->SetProperty(di_device,(const GUID*)4,&range.header))<0)goto fail;
        range.min=range.max=-12345;
        if((result=di_device->v->GetProperty(di_device,(const GUID*)4,&range.header))<0)goto fail;
        if(range.min!=0||range.max!=65535){result=E_FAIL;goto fail;}
        for(unsigned property=5;property<=6;property++){
            DiPropertyNumber value={{sizeof(value),sizeof(value.header),i*4,1},property==5?0:10000};DWORD expected=value.value;
            if((result=di_device->v->SetProperty(di_device,(const GUID*)(ULONG_PTR)property,&value.header))<0)goto fail;
            value.value=0xdeadbeef;
            if((result=di_device->v->GetProperty(di_device,(const GUID*)(ULONG_PTR)property,&value.header))<0)goto fail;
            if(value.value!=expected){result=E_FAIL;goto fail;}
        }
    }
    if((result=di_device->v->Acquire(di_device))>=0)return result;
fail:
    di_device->v->Release(di_device);di_device=NULL;return result;
}
static void close_di_state(void){if(di_device){di_device->v->Unacquire(di_device);di_device->v->Release(di_device);di_device=NULL;}}
static HRESULT read_di_state(Input*input,SkinosDiState*state){
    if(!di_device){HRESULT result=open_di_state(input);if(result<0)return result;}
    HRESULT polled=di_device->v->Poll(di_device);
    if(polled<0)di_device->v->Acquire(di_device);
    memset(state,0xa5,sizeof(*state));
    return di_device->v->GetDeviceState(di_device,sizeof(*state),state);
}
static void emit_di_state(const SkinosDiState*s){
    out("{\"x\":");signed_num(s->x);out(",\"y\":");signed_num(s->y);out(",\"z\":");signed_num(s->z);
    out(",\"rx\":");signed_num(s->rx);out(",\"ry\":");signed_num(s->ry);out(",\"pov\":");num(s->pov);
    out(",\"buttons\":[");for(unsigned i=0;i<10;i++){if(i)out(",");num(s->buttons[i]);}out("]}");
}
#endif
