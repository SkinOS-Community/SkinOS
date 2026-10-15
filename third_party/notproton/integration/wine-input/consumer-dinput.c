/* Standalone experimental consumer. It does not replace live consumer.c or
 * artifacts; uses stock public DirectInput and XInput DLLs, no Steam APIs. */
#define mainCRTStartup original_transport_entry
#include "consumer.c"
#undef mainCRTStartup
#include "dinput-state.h"
void mainCRTStartup(void){
    HMODULE ntdll=GetModuleHandleW(L"ntdll.dll");
    if(!ntdll||!GetProcAddress(ntdll,"wine_get_version")){out("{\"event\":\"wine_required\",\"ok\":false}\n");ExitProcess(1);}
    HMODULE x=LoadLibraryW(L"xinput1_4.dll"),d=LoadLibraryW(L"dinput8.dll");
    GetState get=x?(GetState)GetProcAddress(x,(const char*)100):NULL;BOOL extended=get!=NULL;
    if(!get&&x)get=(GetState)GetProcAddress(x,"XInputGetState");
    CreateInput create=d?(CreateInput)GetProcAddress(d,"DirectInput8Create"):NULL;Input*input=NULL;
    HRESULT hr=create?create(GetModuleHandleW(NULL),0x0800,&iid_input8w,(void**)&input,NULL):E_FAIL;
    if(!get||hr<0||!input){out("{\"event\":\"transport_consumer_init\",\"ok\":false}\n");ExitProcess(1);}
    out("{\"event\":\"transport_consumer_ready\",\"transport_only\":true,\"steam_input_verified\":false,\"dinput_state_probe\":true,\"xinput_extended\":");out(extended?"true":"false");out("}\n");
    DWORD failure=0;
    for(;;){
        SkinosInputFrame f;DWORD have=0;
        while(have<sizeof(f)){DWORD n=0;if(!ReadFile(GetStdHandle(STD_INPUT_HANDLE),(BYTE*)&f+have,sizeof(f)-have,&n,NULL)||!n)break;have+=n;}
        if(!have)break;
        if(have!=sizeof(f)||f.magic!=SKINOS_INPUT_MAGIC||f.version!=1||f.slot>=4||!((f.kind==2&&f.payload_len==20)||(f.kind==3&&f.payload_len==0))){failure=1;break;}
        if(f.kind==3)close_di_state();
        DWORD began=GetTickCount(),code=1167,connected=0;BOOL ok=FALSE,di_match=FALSE;HRESULT di_result=E_FAIL;
        XINPUT_STATE actual={0};SkinosDiState observed={0},expected={0};skinos_di_expect(f.payload,&expected);
        do{
            code=get(f.slot,&actual);connected=0;for(DWORD slot=0;slot<4;slot++){XINPUT_STATE check;if(get(slot,&check)==0)++connected;}
            dinput_total=dinput_owned=dinput_owned_slots=0;di_instance_found=FALSE;
            hr=input->v->EnumDevices(input,4,enumerate_di_state,input,1);
            if(f.kind==2&&hr>=0&&dinput_total==1&&dinput_owned==1&&dinput_owned_slots==(1u<<f.slot)&&di_instance_found){
                di_result=read_di_state(input,&observed);di_match=di_result>=0&&skinos_di_equal_semantic(&observed,&expected,f.payload);
            }else di_match=FALSE;
            ok=hr>=0&&(f.kind==3?(connected==0&&dinput_total==0):(!code&&connected==1&&matches(&f,&actual,extended)&&di_match));
            if(ok)break;Sleep(25);
        }while(GetTickCount()-began<5000);
        out("{\"event\":\"transport_comparison\",\"transport_only\":true,\"steam_input_verified\":false,\"ok\":");out(ok?"true":"false");
        out(",\"kind\":");num(f.kind);out(",\"slot\":");num(f.slot);out(",\"xinput_result\":");num(code);out(",\"dinput_enum_result\":");num((DWORD)hr);
        out(",\"dinput_state_result\":");num((DWORD)di_result);out(",\"dinput_match\":");out(di_match?"true":"false");
        out(",\"dinput_owned_devices\":");num(dinput_owned);out(",\"dinput_total_devices\":");num(dinput_total);
        out(",\"dinput_center_rounding_mask\":");num(di_result>=0?skinos_di_center_rounding(&observed,f.payload):0);
        out(",\"dinput_trigger_mode\":\"combined_lt_minus_rt\",\"dinput_guide_exposed\":false,\"observed_di\":");
        if(di_result>=0)emit_di_state(&observed);else out("null");out(",\"expected_di\":");emit_di_state(&expected);out("}\n");
        if(!ok)failure=1;
    }
    close_di_state();input->v->Release(input);ExitProcess(failure);
}
