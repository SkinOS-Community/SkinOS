/* Owned Wine-only input transport. It never invents game/SDK/controller state. */
#define UNICODE
#define _UNICODE
#define WIN32_LEAN_AND_MEAN
#include <stdarg.h>
#include <stddef.h>
#include "windows.h"
#include "winreg.h"
#include "winioctl.h"
#include "setupapi.h"
#include "newdev.h"
#include "winsvc.h"
#include "cfgmgr32.h"
#include "shellapi.h"
#include "protocol.h"
#include "descriptor.h"
#include "feedback.h"

static const GUID system_class = {0x4d36e97d,0xe325,0x11ce,{0xbf,0xc1,0x08,0x00,0x2b,0xe1,0x03,0x18}};
static const WCHAR instance[] = L"ROOT\\SKINOSINPUTBUS\\0000";
static const WCHAR hardware[] = L"Root\\SkinOsInputBus\0";
static BOOL owned[SKINOS_INPUT_SLOTS];
static DWORD output_stream=STD_OUTPUT_HANDLE;
static BOOL feedback_mode;
#define SKINOS_FEEDBACK_BATCH_MAX 8u

static int same(const void *a,const void *b,size_t n) { const unsigned char *x=a,*y=b;while(n--)if(*x++!=*y++)return 0;return 1; }
static BOOL eq(const WCHAR *a,const WCHAR *b) { while(*a&&*a==*b){++a;++b;}return *a==*b; }
static void out(const char *s) { DWORD n=0,len=0;while(s[len])++len;while(n<len){DWORD written=0;if(!WriteFile(GetStdHandle(output_stream),s+n,len-n,&written,NULL)||!written)break;n+=written;} }
static void num(DWORD x) { char s[12];unsigned n=0;do{s[n++]=(char)('0'+x%10);x/=10;}while(x);while(n){char c[2]={s[--n],0};out(c);} }
static void event(const char *name,BOOL ok,DWORD code,DWORD slot,DWORD kind) {
    out("{\"event\":\"");out(name);out("\",\"ok\":");out(ok?"true":"false");out(",\"win32_error\":");num(code);
    out(",\"slot\":");num(slot);out(",\"kind\":");num(kind);out("}\n");
}
static BOOL wine_only(void) { HMODULE n=GetModuleHandleW(L"ntdll.dll");return n&&GetProcAddress(n,"wine_get_version")!=NULL; }
static BOOL property_matches(HDEVINFO set, SP_DEVINFO_DATA *dev, DWORD prop, const WCHAR *expected) {
    WCHAR value[256];DWORD type=0,size=0;memset(value,0,sizeof(value));
    if(!SetupDiGetDeviceRegistryPropertyW(set,dev,prop,&type,(BYTE*)value,sizeof(value),&size))return FALSE;
    if(!eq(value,expected)){SetLastError(ERROR_ACCESS_DENIED);return FALSE;}return TRUE;
}
/* Real status, with explicit availability/error fields. Service state zero or
 * devnode status zero is never substituted for an unavailable query. */
static void diagnose(void) {
    SERVICE_STATUS_PROCESS status={0};DWORD bytes=0,error=0;BOOL service_found=FALSE,queried=FALSE;
    SC_HANDLE scm=OpenSCManagerW(NULL,NULL,SC_MANAGER_CONNECT);
    if(!scm)error=GetLastError();
    else{
        SC_HANDLE service=OpenServiceW(scm,L"SkinOsInputBus",SERVICE_QUERY_STATUS);
        if(!service)error=GetLastError();
        else{service_found=TRUE;queried=QueryServiceStatusEx(service,SC_STATUS_PROCESS_INFO,(BYTE*)&status,sizeof(status),&bytes);
            if(!queried)error=GetLastError();CloseServiceHandle(service);}
        CloseServiceHandle(scm);
    }
    DEVINST node=0;ULONG flags=0xffffffffu,problem=0xffffffffu;
    CONFIGRET located=CM_Locate_DevNodeW(&node,(WCHAR*)instance,0); /* normal, no flags */
    CONFIGRET got=located==CR_SUCCESS?CM_Get_DevNode_Status(&flags,&problem,node,0):located;
    BOOL written=got==CR_SUCCESS&&flags!=0xffffffffu&&problem!=0xffffffffu;
    out("{\"event\":\"wine_input_driver_status\",\"service_found\":");out(service_found?"true":"false");
    out(",\"service_status_available\":");out(queried?"true":"false");out(",\"service_query_error\":");num(error);
    out(",\"service_state\":");if(queried)num(status.dwCurrentState);else out("null");
    out(",\"service_win32_exit\":");if(queried)num(status.dwWin32ExitCode);else out("null");
    out(",\"service_specific_exit\":");if(queried)num(status.dwServiceSpecificExitCode);else out("null");
    out(",\"service_pid\":");if(queried)num(status.dwProcessId);else out("null");
    out(",\"service_checkpoint\":");if(queried)num(status.dwCheckPoint);else out("null");
    out(",\"service_wait_hint_ms\":");if(queried)num(status.dwWaitHint);else out("null");
    out(",\"cm_locate_result\":");num(located);out(",\"cm_status_result\":");num(got);
    out(",\"cm_status_written\":");out(written?"true":"false");
    out(",\"devnode_status\":");if(written)num(flags);else out("null");
    out(",\"devnode_problem\":");if(written)num(problem);else out("null");
    out(",\"devnode_started\":");if(written)out((flags&DN_STARTED)?"true":"false");else out("null");out("}\n");
}
static BOOL service_owned(SC_HANDLE service) {
    DWORD size=0;QueryServiceConfigW(service,NULL,0,&size);
    if(!size||size>65536)return FALSE;
    QUERY_SERVICE_CONFIGW *config=HeapAlloc(GetProcessHeap(),0,size);if(!config)return FALSE;
    WCHAR expected[512];UINT n=GetSystemDirectoryW(expected,480);BOOL ok=FALSE;
    if(n&&n<480&&QueryServiceConfigW(service,config,size,&size)){
        const WCHAR suffix[]=L"\\drivers\\skinosinput.sys";
        for(UINT i=0;i<sizeof(suffix)/sizeof(WCHAR);i++)expected[n+i]=suffix[i];
        ok=config->dwServiceType==SERVICE_KERNEL_DRIVER&&config->dwStartType==SERVICE_DEMAND_START&&
           config->lpBinaryPathName&&lstrcmpiW(config->lpBinaryPathName,expected)==0;
    }
    HeapFree(GetProcessHeap(),0,config);return ok;
}
static BOOL wait_start_lock(SC_HANDLE scm,DWORD remaining_ms) {
    union { QUERY_SERVICE_LOCK_STATUSW lock; BYTE data[1024]; } buffer;
    memset(&buffer,0,sizeof(buffer));DWORD needed=0;
    SetLastError(0);
    BOOL available=QueryServiceLockStatusW(scm,&buffer.lock,sizeof(buffer),&needed);
    DWORD query_error=available?0:GetLastError();
    /* In pinned Wine this public query is NOT_IMPLEMENTED. The actual
     * services.exe startup event is set only after autostart releases its
     * internal service_start_lock; do not infer unlocked from query failure. */
    HANDLE startup=OpenEventW(SYNCHRONIZE,FALSE,L"__wine_SvcctlStarted");
    DWORD event_error=startup?0:GetLastError(),observed=startup?WaitForSingleObject(startup,0):WAIT_FAILED;
    out("{\"event\":\"wine_input_scm_lock\",\"query_available\":");out(available?"true":"false");
    out(",\"query_error\":");if(available||query_error)num(query_error);else out("null");
    out(",\"query_error_provided\":");out(available||query_error?"true":"false");
    out(",\"start_error\":1055,\"database_locked\":");if(available)out(buffer.lock.fIsLocked?"true":"false");else out("null");
    out(",\"lock_duration_seconds\":");if(available)num(buffer.lock.dwLockDuration);else out("null");
    out(",\"startup_event_available\":");out(startup?"true":"false");out(",\"startup_event_error\":");num(event_error);
    out(",\"startup_observation\":");num(observed);out("}\n");
    BOOL retry=FALSE;
    if(startup&&observed==WAIT_TIMEOUT){
        DWORD waited=WaitForSingleObject(startup,remaining_ms);
        out("{\"event\":\"wine_input_scm_startup_wait\",\"wait_result\":");num(waited);out("}\n");
        retry=waited==WAIT_OBJECT_0;
    }else if(remaining_ms){
        /* The caller got actual1055. Pinned services/rpc.c emits that only
         * when service_start_lock remains occupied for3s. This is sufficient
         * evidence to retry, even after autostart and with a stub lock query;
         * never release, replace, or take over the SCM's lock. */
        Sleep(remaining_ms<250?remaining_ms:250);retry=TRUE;
    }
    if(startup)CloseHandle(startup);return retry;
}
static DWORD start_owned(void) {
    /* Registration handles are closed, then identity is independently read
     * through a fresh list which is also closed before any SCM start call. */
    SP_DEVINFO_DATA dev={0};dev.cbSize=sizeof(dev);
    HDEVINFO set=SetupDiCreateDeviceInfoList(&system_class,NULL);if(set==INVALID_HANDLE_VALUE)return GetLastError();
    BOOL identity=SetupDiOpenDeviceInfoW(set,instance,NULL,0,&dev)&&
        property_matches(set,&dev,SPDRP_HARDWAREID,L"Root\\SkinOsInputBus")&&
        property_matches(set,&dev,SPDRP_SERVICE,L"SkinOsInputBus");
    SetupDiDestroyDeviceInfoList(set);if(!identity)return ERROR_ACCESS_DENIED;
    SC_HANDLE scm=OpenSCManagerW(NULL,NULL,SC_MANAGER_CONNECT|SC_MANAGER_QUERY_LOCK_STATUS);if(!scm)return GetLastError();
    SC_HANDLE service=OpenServiceW(scm,L"SkinOsInputBus",SERVICE_START|SERVICE_QUERY_STATUS|SERVICE_QUERY_CONFIG|SERVICE_USER_DEFINED_CONTROL);
    DWORD error=0;
    if(!service)error=GetLastError();
    else if(!service_owned(service))error=ERROR_ACCESS_DENIED;
    else{
        BOOL requested=FALSE;DWORD start_error=0,begin=GetTickCount();
        for(DWORD attempt=0;attempt<3;attempt++){
            requested=StartServiceW(service,0,NULL);start_error=requested?0:GetLastError();
            event("wine_input_start_request",requested||start_error==ERROR_SERVICE_ALREADY_RUNNING,start_error,0,attempt+1);
            if(requested||start_error!=ERROR_SERVICE_DATABASE_LOCKED)break;
            DWORD elapsed=GetTickCount()-begin;
            if(elapsed>=15000||!wait_start_lock(scm,15000-elapsed))break;
        }
        if(!requested&&start_error!=ERROR_SERVICE_ALREADY_RUNNING)error=start_error;
        else{
            SERVICE_STATUS_PROCESS status={0};DWORD needed=0,started=GetTickCount();
            for(;;){
                if(!QueryServiceStatusEx(service,SC_STATUS_PROCESS_INFO,(BYTE*)&status,sizeof(status),&needed)){error=GetLastError();break;}
                if(status.dwCurrentState==SERVICE_RUNNING)break;
                if(status.dwCurrentState==SERVICE_STOPPED){error=status.dwWin32ExitCode?status.dwWin32ExitCode:ERROR_SERVICE_NOT_ACTIVE;break;}
                if(GetTickCount()-started>=5000){error=ERROR_TIMEOUT;break;}Sleep(50);
            }
            if(!error){
                /* Exact Wine SetupDiInstallDevice -> winedevice sequence.
                 * CM_Reenumerate_DevNode is a failing stub in the pinned Wine.
                 * Control128 enumerates ROOT devices for this service only. */
                SERVICE_STATUS status2;BOOL enumerated=ControlService(service,128,&status2);
                if(!enumerated)error=GetLastError();
                event("wine_input_root_enumeration_request",enumerated,error,0,0);
            }
        }
    }
    if(service)CloseServiceHandle(service);CloseServiceHandle(scm);return error;
}
static HANDLE open_bus(DWORD timeout_ms,DWORD *error) {
    DWORD started=GetTickCount();
    for(;;){
        HANDLE bus=CreateFileW(L"\\\\.\\SkinOsInputBus",GENERIC_WRITE,0,NULL,OPEN_EXISTING,0,NULL);
        if(bus!=INVALID_HANDLE_VALUE){*error=0;return bus;}
        *error=GetLastError();
        /* Sharing/access errors mean another owner or permissions, not a
         * startup race. Never take over an existing producer connection. */
        if(*error!=ERROR_FILE_NOT_FOUND&&*error!=ERROR_PATH_NOT_FOUND&&*error!=ERROR_NOT_READY&&*error!=ERROR_SERVICE_NOT_ACTIVE)
            return INVALID_HANDLE_VALUE;
        if(GetTickCount()-started>=timeout_ms)return INVALID_HANDLE_VALUE;
        Sleep(50);
    }
}
static BOOL wait_root(HANDLE bus,DWORD timeout_ms,DWORD *error) {
    DWORD start=GetTickCount(),returned=0;SkinosInputStatus s={0};
    for(;;){
        if(!DeviceIoControl(bus,SKINOS_INPUT_STATUS_IOCTL,NULL,0,&s,sizeof(s),&returned,NULL)){*error=GetLastError();break;}
        if(returned!=sizeof(s)||s.magic!=SKINOS_INPUT_MAGIC||s.version!=1){*error=ERROR_INVALID_DATA;break;}
        if(s.root_present&&s.root_started&&!s.root_removed){*error=0;break;}
        if(GetTickCount()-start>=timeout_ms){*error=ERROR_NOT_READY;break;}Sleep(50);
    }
    out("{\"event\":\"wine_input_root_readiness\",\"ok\":");out(*error?"false":"true");out(",\"win32_error\":");num(*error);
    out(",\"state_read\":");out(returned==sizeof(s)&&s.magic==SKINOS_INPUT_MAGIC&&s.version==1?"true":"false");
    out(",\"root_present\":");num(s.root_present);out(",\"root_started\":");num(s.root_started);
    out(",\"root_removed\":");num(s.root_removed);out(",\"children_mask\":");num(s.children_mask);out("}\n");return *error==0;
}
static DWORD activate_owned(void) {
    DWORD error=start_owned();
    if(!error){
        HANDLE bus=open_bus(5000,&error);
        if(bus!=INVALID_HANDLE_VALUE){wait_root(bus,5000,&error);CloseHandle(bus);}
    }
    diagnose();event("wine_input_activate",error==0,error,0,0);return error;
}
static DWORD install(const WCHAR *inf,BOOL activate) {
    WCHAR full[1024];DWORD length=GetFullPathNameW(inf,1024,full,NULL),error=0;BOOL reboot=FALSE,created=FALSE;
    SP_DEVINFO_DATA dev={0};dev.cbSize=sizeof(dev);
    if(!length||length>=1024||GetFileAttributesW(full)==INVALID_FILE_ATTRIBUTES)return ERROR_FILE_NOT_FOUND;
    HDEVINFO set=SetupDiCreateDeviceInfoList(&system_class,NULL);
    if(set==INVALID_HANDLE_VALUE)return GetLastError();
    if(SetupDiOpenDeviceInfoW(set,instance,NULL,0,&dev)) {
        if(!property_matches(set,&dev,SPDRP_HARDWAREID,L"Root\\SkinOsInputBus")){error=ERROR_ALREADY_EXISTS;goto done;}
        if(!property_matches(set,&dev,SPDRP_SERVICE,L"SkinOsInputBus")){
            DWORD property_error=GetLastError();
            if(property_error!=ERROR_INVALID_DATA&&property_error!=ERROR_FILE_NOT_FOUND){error=ERROR_ALREADY_EXISTS;goto done;}
        }
    } else {
        if(!SetupDiCreateDeviceInfoW(set,instance,&system_class,L"SkinOS Wine input transport",NULL,0,&dev)||
           !SetupDiSetDeviceRegistryPropertyW(set,&dev,SPDRP_HARDWAREID,(const BYTE*)hardware,sizeof(hardware))||
           !SetupDiCallClassInstaller(DIF_REGISTERDEVICE,set,&dev)){error=GetLastError();goto done;}
        created=TRUE;
    }
    if(!UpdateDriverForPlugAndPlayDevicesW(NULL,L"Root\\SkinOsInputBus",full,INSTALLFLAG_FORCE|INSTALLFLAG_NONINTERACTIVE,&reboot)) {
        error=GetLastError();if(created)SetupDiCallClassInstaller(DIF_REMOVE,set,&dev);
    }
done:
    event("wine_input_install_api",error==0,error,0,0);
    SetupDiDestroyDeviceInfoList(set);
    event("wine_input_registration",error==0,error,0,0);
    if(activate){if(!error)error=activate_owned();event("wine_input_install",error==0,error,0,0);}
    return error;
}
static DWORD uninstall(void) {
    SP_DEVINFO_DATA dev={0};dev.cbSize=sizeof(dev);DWORD error=0;BOOL node_found=FALSE;
    SC_HANDLE scm=NULL,service=NULL;
    HDEVINFO set=SetupDiCreateDeviceInfoList(&system_class,NULL);
    if(set==INVALID_HANDLE_VALUE)return GetLastError();
    node_found=SetupDiOpenDeviceInfoW(set,instance,NULL,0,&dev);
    if(!node_found){DWORD open_error=GetLastError();
        if(open_error!=ERROR_NO_SUCH_DEVINST&&open_error!=ERROR_FILE_NOT_FOUND&&open_error!=ERROR_NOT_FOUND){error=open_error;goto done;}}
    if(node_found){
        if(!property_matches(set,&dev,SPDRP_HARDWAREID,L"Root\\SkinOsInputBus")){error=ERROR_ACCESS_DENIED;goto done;}
        if(!property_matches(set,&dev,SPDRP_SERVICE,L"SkinOsInputBus")){
            DWORD property_error=GetLastError();
            if(property_error!=ERROR_INVALID_DATA&&property_error!=ERROR_FILE_NOT_FOUND){error=ERROR_ACCESS_DENIED;goto done;}
        }
    }
    scm=OpenSCManagerW(NULL,NULL,SC_MANAGER_CONNECT);
    if(!scm){error=GetLastError();goto done;}
    service=OpenServiceW(scm,L"SkinOsInputBus",DELETE|SERVICE_STOP|SERVICE_QUERY_CONFIG);
    if(!service){error=GetLastError();if(error==ERROR_SERVICE_DOES_NOT_EXIST)error=0;else goto done;}
    else if(!service_owned(service)){error=ERROR_ACCESS_DENIED;goto done;}
    if(node_found&&!SetupDiCallClassInstaller(DIF_REMOVE,set,&dev)){error=GetLastError();goto done;}
    if(service){
        SERVICE_STATUS status;
        if(!ControlService(service,SERVICE_CONTROL_STOP,&status)){
            DWORD stop_error=GetLastError();if(stop_error!=ERROR_SERVICE_NOT_ACTIVE){error=stop_error;goto done;}
        }
        if(!DeleteService(service))error=GetLastError();
    }
done:
    if(service)CloseServiceHandle(service);if(scm)CloseServiceHandle(scm);
    SetupDiDestroyDeviceInfoList(set);event("wine_input_uninstall",error==0,error,0,0);return error;
}
static BOOL valid(const SkinosInputFrame *f) {
    if(f->magic!=SKINOS_INPUT_MAGIC||f->version!=SKINOS_INPUT_VERSION||f->slot>=SKINOS_INPUT_SLOTS||
       f->vendor>65535||f->product>65535||f->device_version>65535)return FALSE;
    if(f->kind==1)return f->payload_len==sizeof(skinos_input_descriptor)&&same(f->payload,skinos_input_descriptor,sizeof(skinos_input_descriptor));
    if(f->kind==2)return f->payload_len==20;
    return f->kind==3&&f->payload_len==0;
}
static BOOL submit(HANDLE bus,const SkinosInputFrame *f,DWORD *error) {
    DWORD returned=0;BOOL ok=DeviceIoControl(bus,SKINOS_INPUT_IOCTL,(void*)f,sizeof(*f),NULL,0,&returned,NULL);
    *error=ok?0:GetLastError();return ok;
}
static BOOL cleanup(HANDLE bus) {
    BOOL complete=TRUE;
    for(DWORD i=0;i<SKINOS_INPUT_SLOTS;i++)if(owned[i]) {
        SkinosInputFrame f={0};DWORD error=0;f.magic=SKINOS_INPUT_MAGIC;f.version=1;f.kind=3;f.slot=i;
        BOOL ok=submit(bus,&f,&error);event("wine_input_cleanup",ok,error,i,3);if(!ok)complete=FALSE;owned[i]=FALSE;
    }
    return complete;
}
static BOOL feedback_validate_batch(const SkinosFeedbackFrame *rows,DWORD returned,DWORD *count) {
    uint64_t previous=0;DWORD session=0;
    if(returned>SKINOS_FEEDBACK_BATCH_MAX*sizeof(*rows)||returned%sizeof(*rows))return FALSE;
    *count=returned/sizeof(*rows);
    for(DWORD i=0;i<*count;++i) {
        const SkinosFeedbackFrame*f=rows+i;
        uint64_t sequence=((uint64_t)f->sequence_high<<32)|f->sequence_low;
        if(f->magic!=SKINOS_FEEDBACK_MAGIC||f->version!=1||f->kind!=1||f->byte_size!=sizeof(*f)||
           !f->control_session||f->slot>=16||!f->generation||!sequence||sequence<=previous||
           (session&&f->control_session!=session)||f->left>65535||f->right>65535||f->report_id!=1)return FALSE;
        previous=sequence;session=f->control_session;
    }
    return TRUE;
}
static void feedback_failed_reply(DWORD error) {
    out("{\"event\":\"wine_input_feedback\",\"ok\":false,\"win32_error\":");num(error);
    out(",\"available\":false,\"batch_version\":1,\"records\":[],\"drained\":false,\"loss_latched\":");
    /* This owned IOCTL uses STATUS_DATA_ERROR only for sticky queue loss.
     * Wine 290276fb dlls/ntdll/error.h maps c000003e to ERROR_CRC (23). */
    out(error==ERROR_CRC?"true":"false");out("}\n");
}
static void feedback_record(const SkinosFeedbackFrame*f) {
    out("{\"kind\":1,\"byte_size\":48,\"control_session\":");num(f->control_session);
    out(",\"slot\":");num(f->slot);out(",\"generation\":");num(f->generation);
    out(",\"sequence_low\":");num(f->sequence_low);out(",\"sequence_high\":");num(f->sequence_high);
    out(",\"left\":");num(f->left);out(",\"right\":");num(f->right);out(",\"report_id\":");num(f->report_id);
    out(",\"hid_output_dequeued\":true,\"sdk_dispatched\":false,\"physical_device_verified\":false}");
}
static DWORD feedback_read(HANDLE bus) {
    SkinosFeedbackFrame rows[SKINOS_FEEDBACK_BATCH_MAX]={0};DWORD returned=0,count=0;
    BOOL ok=DeviceIoControl(bus,SKINOS_FEEDBACK_IOCTL,NULL,0,rows,sizeof(rows),&returned,NULL);
    DWORD error=ok?0:GetLastError();
    if(!ok&&error==ERROR_NO_MORE_ITEMS&&!returned){ok=TRUE;error=0;}
    if(!ok){feedback_failed_reply(error);return error;}
    /* Nothing, including a valid prefix of a malformed batch, is published
     * until ALL returned records pass validation. Exactly one IOCTL per query. */
    if(!feedback_validate_batch(rows,returned,&count)) {
        feedback_failed_reply(ERROR_INVALID_DATA);return ERROR_INVALID_DATA;
    }
    out("{\"event\":\"wine_input_feedback\",\"ok\":true,\"win32_error\":0,\"available\":");out(count?"true":"false");
    out(",\"batch_version\":1,\"records\":[");
    for(DWORD i=0;i<count;++i){if(i)out(",");feedback_record(rows+i);}
    out("],\"drained\":");out(count<SKINOS_FEEDBACK_BATCH_MAX?"true":"false");out("}\n");return 0;
}
static BOOL feedback_create_ack(HANDLE bus,DWORD slot,DWORD*error) {
    SkinosFeedbackBinding b={0};DWORD bytes=0;
    BOOL ok=DeviceIoControl(bus,SKINOS_FEEDBACK_BINDING_IOCTL,&slot,sizeof(slot),&b,sizeof(b),&bytes,NULL);
    *error=ok?0:GetLastError();
    if(ok&&(bytes!=sizeof(b)||b.magic!=SKINOS_FEEDBACK_MAGIC||b.version!=1||b.byte_size!=sizeof(b)||
            !b.control_session||b.slot!=slot||!b.generation)){ok=FALSE;*error=ERROR_INVALID_DATA;}
    if(!ok){event("wine_input_ack",FALSE,*error,slot,1);return FALSE;}
    out("{\"event\":\"wine_input_ack\",\"ok\":true,\"win32_error\":0,\"kind\":1,\"slot\":");num(slot);
    out(",\"control_session\":");num(b.control_session);out(",\"generation\":");num(b.generation);
    out(",\"driver_binding_verified\":true}\n");return TRUE;
}
static DWORD feed_handle(HANDLE bus) {
    DWORD open_error=0;
    if(!wait_root(bus,5000,&open_error)){CloseHandle(bus);diagnose();return open_error;}
    event("wine_input_ready",TRUE,0,0,0);
    DWORD result=0;
    for(;;) {
        SkinosInputFrame frame;DWORD have=0;BOOL eof=FALSE;
        while(have<sizeof(frame)) {
            DWORD count=0;BOOL ok=ReadFile(GetStdHandle(STD_INPUT_HANDLE),(BYTE*)&frame+have,sizeof(frame)-have,&count,NULL);
            if(!ok){DWORD error=GetLastError();if(error!=ERROR_BROKEN_PIPE){result=error;event("wine_input_read",FALSE,error,0,0);}eof=TRUE;break;}
            if(!count){eof=TRUE;break;}have+=count;
        }
        if(eof){if(have){result=ERROR_INVALID_DATA;event("wine_input_truncated_frame",FALSE,result,0,0);}break;}
        /* Experimental control command only: no second bus handle, thread,
         * output-writer race or pending feedback IRP. Existing input frames
         * remain unchanged and keep their real submit/ACK path below. */
        if(feedback_mode&&frame.kind==4) {
            if(frame.magic!=SKINOS_INPUT_MAGIC||frame.version!=1||frame.slot||frame.payload_len) {
                result=ERROR_INVALID_DATA;event("wine_input_feedback_query",FALSE,result,frame.slot,frame.kind);break;
            }
            /* A failed rumble dequeue disables the caller's feedback path,
             * not the original input stream. feedback_read already emits its
             * truthful failed reply; a later input operation gets its own real
             * driver result. Malformed control/input frames still fail above. */
            (void)feedback_read(bus);continue;
        }
        if(!valid(&frame)){result=ERROR_INVALID_DATA;event("wine_input_invalid_frame",FALSE,result,frame.slot,frame.kind);break;}
        if((frame.kind==1&&owned[frame.slot])||(frame.kind!=1&&!owned[frame.slot])) {
            result=ERROR_INVALID_STATE;event("wine_input_invalid_lifecycle",FALSE,result,frame.slot,frame.kind);break;
        }
        DWORD error;BOOL ok=submit(bus,&frame,&error);
        if(ok&&feedback_mode&&frame.kind==1)ok=feedback_create_ack(bus,frame.slot,&error);
        else event("wine_input_ack",ok,error,frame.slot,frame.kind);
        if(!ok){result=error;break;}
        if(frame.kind==1)owned[frame.slot]=TRUE;
        else if(frame.kind==3)owned[frame.slot]=FALSE;
    }
    if(!cleanup(bus)&&!result)result=ERROR_GEN_FAILURE;
    CloseHandle(bus);event("wine_input_closed",result==0,result,0,0);return result;
}
static DWORD feed(void) {
    DWORD error=0;HANDLE bus=open_bus(5000,&error);
    if(bus==INVALID_HANDLE_VALUE){diagnose();event("wine_input_open",FALSE,error,0,0);return error;}
    return feed_handle(bus);
}

typedef struct EnsureContext {
    WCHAR inf[1024],payload_sys[1024],installed_sys[512];
    HANDLE bus;
} EnsureContext;
static BOOL absent_error(DWORD error){return error==ERROR_FILE_NOT_FOUND||error==ERROR_PATH_NOT_FOUND||error==ERROR_NO_SUCH_DEVINST||error==ERROR_NOT_FOUND;}
static DWORD regular_file(const WCHAR*path,BOOL allow_absent,BOOL*present){
    DWORD attributes=GetFileAttributesW(path);*present=FALSE;
    if(attributes==INVALID_FILE_ATTRIBUTES){DWORD error=GetLastError();return allow_absent&&absent_error(error)?0:error;}
    if(attributes&(FILE_ATTRIBUTE_DIRECTORY|FILE_ATTRIBUTE_REPARSE_POINT))return ERROR_ACCESS_DENIED;
    *present=TRUE;return 0;
}
static DWORD file_size(HANDLE file,LONGLONG*size){
    LARGE_INTEGER value;if(!GetFileSizeEx(file,&value))return GetLastError();
    if(value.QuadPart<=0||value.QuadPart>16*1024*1024)return ERROR_INVALID_DATA;
    *size=value.QuadPart;return 0;
}
static DWORD same_files(const WCHAR*a,const WCHAR*b,BOOL*equal){
    HANDLE one=INVALID_HANDLE_VALUE,two=INVALID_HANDLE_VALUE;DWORD error=0;LONGLONG first=0,second=0;
    BYTE buffer[2][512];*equal=FALSE;
    one=CreateFileW(a,GENERIC_READ,FILE_SHARE_READ,NULL,OPEN_EXISTING,0,NULL);if(one==INVALID_HANDLE_VALUE)return GetLastError();
    two=CreateFileW(b,GENERIC_READ,FILE_SHARE_READ,NULL,OPEN_EXISTING,0,NULL);if(two==INVALID_HANDLE_VALUE){error=GetLastError();goto done;}
    if((error=file_size(one,&first))||(error=file_size(two,&second)))goto done;
    if(first!=second)goto done;
    for(LONGLONG offset=0;offset<first;){
        DWORD count1=0,count2=0,amount=(DWORD)((first-offset)<512?(first-offset):512);
        if(!ReadFile(one,buffer[0],amount,&count1,NULL)||!ReadFile(two,buffer[1],amount,&count2,NULL)){error=GetLastError();goto done;}
        if(count1!=amount||count2!=amount){error=ERROR_READ_FAULT;goto done;}
        if(!same(buffer[0],buffer[1],amount))goto done;offset+=amount;
    }
    *equal=TRUE;
done:
    if(two!=INVALID_HANDLE_VALUE)CloseHandle(two);CloseHandle(one);return error;
}
static DWORD payload_paths(EnsureContext*c,const WCHAR*inf){
    DWORD n=GetFullPathNameW(inf,1024,c->inf,NULL),error=0;BOOL present;
    if(!n||n>=1024)return ERROR_INVALID_NAME;
    UINT base=0;for(UINT i=0;i<n;i++)if(c->inf[i]=='\\'||c->inf[i]=='/')base=i+1;
    if(lstrcmpiW(c->inf+base,L"skinosinput.inf"))return ERROR_INVALID_NAME;
    memcpy(c->payload_sys,c->inf,(n+1)*sizeof(WCHAR));
    memcpy(c->payload_sys+base,L"skinosinput.sys",sizeof(L"skinosinput.sys"));
    UINT system=GetSystemDirectoryW(c->installed_sys,480);if(!system||system>=480)return ERROR_INVALID_NAME;
    memcpy(c->installed_sys+system,L"\\drivers\\skinosinput.sys",sizeof(L"\\drivers\\skinosinput.sys"));
    if((error=regular_file(c->inf,FALSE,&present))||(error=regular_file(c->payload_sys,FALSE,&present)))return error;
    HANDLE file=CreateFileW(c->payload_sys,GENERIC_READ,FILE_SHARE_READ,NULL,OPEN_EXISTING,0,NULL);
    if(file==INVALID_HANDLE_VALUE)return GetLastError();
    LONGLONG size=0;BYTE header[64];DWORD count=0;
    if((error=file_size(file,&size)))goto done;
    if(!ReadFile(file,header,sizeof(header),&count,NULL)||count!=sizeof(header)||header[0]!='M'||header[1]!='Z'){error=ERROR_BAD_EXE_FORMAT;goto done;}
    DWORD pe=(DWORD)header[60]|((DWORD)header[61]<<8)|((DWORD)header[62]<<16)|((DWORD)header[63]<<24);
    if(pe>(DWORD)size-6){error=ERROR_BAD_EXE_FORMAT;goto done;}
    LARGE_INTEGER offset;offset.QuadPart=pe;
    if(!SetFilePointerEx(file,offset,NULL,FILE_BEGIN)||!ReadFile(file,header,6,&count,NULL)||count!=6){error=GetLastError();if(!error)error=ERROR_READ_FAULT;goto done;}
#if defined(__aarch64__) || defined(_M_ARM64)
    const unsigned machine=0xaa64;
#else
    const unsigned machine=0x8664;
#endif
    if(header[0]!='P'||header[1]!='E'||header[2]||header[3]||((unsigned)header[4]|((unsigned)header[5]<<8))!=machine)error=ERROR_BAD_EXE_FORMAT;
done:
    CloseHandle(file);return error;
}
static DWORD inspect_owned(EnsureContext*c,SkinosEnsureSnapshot*s,BOOL check_owner){
    memset(s,0,sizeof(*s));s->identity_verified=1;DWORD error=0;
    HDEVINFO set=SetupDiCreateDeviceInfoList(&system_class,NULL);if(set==INVALID_HANDLE_VALUE)return GetLastError();
    SP_DEVINFO_DATA dev={0};dev.cbSize=sizeof(dev);
    if(SetupDiOpenDeviceInfoW(set,instance,NULL,0,&dev)){
        s->root_present=1;
        if(!property_matches(set,&dev,SPDRP_HARDWAREID,L"Root\\SkinOsInputBus"))s->identity_verified=0;
        else if(!property_matches(set,&dev,SPDRP_SERVICE,L"SkinOsInputBus")){
            DWORD property_error=GetLastError();
            if(property_error!=ERROR_INVALID_DATA&&property_error!=ERROR_FILE_NOT_FOUND)s->identity_verified=0;
        }
    }else{error=GetLastError();if(absent_error(error))error=0;}
    SetupDiDestroyDeviceInfoList(set);if(error)return error;
    SC_HANDLE scm=OpenSCManagerW(NULL,NULL,SC_MANAGER_CONNECT);if(!scm)return GetLastError();
    SC_HANDLE service=OpenServiceW(scm,L"SkinOsInputBus",SERVICE_QUERY_CONFIG);
    if(service){s->service_present=1;if(!service_owned(service))s->identity_verified=0;CloseServiceHandle(service);}
    else{error=GetLastError();if(error==ERROR_SERVICE_DOES_NOT_EXIST)error=0;}
    CloseServiceHandle(scm);if(error)return error;
    if(!s->identity_verified)return 0;
    BOOL present=FALSE,equal=FALSE;
    if((error=regular_file(c->installed_sys,TRUE,&present)))return error;
    s->file_present=present;
    if(present){if((error=same_files(c->installed_sys,c->payload_sys,&equal)))return error;s->file_equal=equal;}
    if(check_owner&&c->bus==INVALID_HANDLE_VALUE){
        c->bus=open_bus(0,&error);
        if(c->bus==INVALID_HANDLE_VALUE){
            if(error==ERROR_SHARING_VIOLATION||error==ERROR_BUSY){s->owner_busy=1;return 0;}
            if(!absent_error(error)&&error!=ERROR_NOT_READY&&error!=ERROR_SERVICE_NOT_ACTIVE)return error;
        }else if(!s->service_present){CloseHandle(c->bus);c->bus=INVALID_HANDLE_VALUE;return ERROR_INVALID_STATE;}
    }
    return 0;
}
static uint32_t ensure_remove(void*opaque){
    EnsureContext*c=opaque;if(c->bus!=INVALID_HANDLE_VALUE){CloseHandle(c->bus);c->bus=INVALID_HANDLE_VALUE;}
    DWORD error=uninstall();if(error)return error;
    DWORD began=GetTickCount();
    for(;;){
        SkinosEnsureSnapshot s;if((error=inspect_owned(c,&s,FALSE)))return error;
        if(!s.service_present&&!s.root_present)return 0;
        if(!s.identity_verified)return ERROR_ACCESS_DENIED;
        if(GetTickCount()-began>=5000)return ERROR_SERVICE_MARKED_FOR_DELETE;Sleep(50);
    }
}
static uint32_t ensure_register(void*opaque){
    EnsureContext*c=opaque;if(c->bus!=INVALID_HANDLE_VALUE){CloseHandle(c->bus);c->bus=INVALID_HANDLE_VALUE;}
    return install(c->inf,FALSE);
}
static uint32_t ensure_verify(void*opaque){
    EnsureContext*c=opaque;SkinosEnsureSnapshot s;DWORD error=inspect_owned(c,&s,FALSE);if(error)return error;
    if(!s.identity_verified||!s.service_present||!s.root_present)return ERROR_ACCESS_DENIED;
    return s.file_equal?0:ERROR_REVISION_MISMATCH;
}
static uint32_t ensure_activate(void*opaque){
    EnsureContext*c=opaque;DWORD error=start_owned();if(error)return error;
    if(c->bus==INVALID_HANDLE_VALUE)c->bus=open_bus(5000,&error);
    if(c->bus==INVALID_HANDLE_VALUE)return error;
    return wait_root(c->bus,5000,&error)?0:error;
}
static uint32_t ensure_feed(void*opaque){
    EnsureContext*c=opaque;HANDLE bus=c->bus;c->bus=INVALID_HANDLE_VALUE;
    output_stream=STD_OUTPUT_HANDLE;
    /* This is the sole stdout readiness sequence, compatible with Relay. */
    return feed_handle(bus);
}
static DWORD ensure_and_feed(const WCHAR*inf){
    EnsureContext*c=HeapAlloc(GetProcessHeap(),HEAP_ZERO_MEMORY,sizeof(*c));if(!c)return ERROR_NOT_ENOUGH_MEMORY;
    c->bus=INVALID_HANDLE_VALUE;output_stream=STD_ERROR_HANDLE;
    DWORD error=payload_paths(c,inf);SkinosEnsureSnapshot snapshot;
    if(!error)error=inspect_owned(c,&snapshot,TRUE);
    if(!error){
        event("wine_input_ensure_plan",TRUE,0,0,(DWORD)skinos_ensure_action(&snapshot));
        const SkinosEnsureOperations operations={ensure_remove,ensure_register,ensure_verify,ensure_activate,ensure_feed};
        error=skinos_ensure_run(&snapshot,&operations,c);
    }
    if(c->bus!=INVALID_HANDLE_VALUE)CloseHandle(c->bus);
    if(error&&output_stream==STD_ERROR_HANDLE){diagnose();event("wine_input_ensure_failed",FALSE,error,0,0);}
    output_stream=STD_OUTPUT_HANDLE;HeapFree(GetProcessHeap(),0,c);return error;
}
static HANDLE lifecycle_lock(DWORD*error){
    /* The Windows object namespace belongs to this WINEPREFIX/wineserver. */
    HANDLE lock=CreateMutexW(NULL,FALSE,L"Local\\SkinOS.WineInput.Lifecycle.v1");
    if(!lock){*error=GetLastError();return NULL;}
    DWORD state=WaitForSingleObject(lock,0);
    if(state!=WAIT_OBJECT_0&&state!=WAIT_ABANDONED){*error=state==WAIT_TIMEOUT?ERROR_SHARING_VIOLATION:GetLastError();CloseHandle(lock);return NULL;}
    *error=0;return lock;
}
void mainCRTStartup(void) {
    int argc=0;WCHAR **argv=CommandLineToArgvW(GetCommandLineW(),&argc);DWORD result=0;HANDLE lock=NULL;
    if(!wine_only()){event("wine_required",FALSE,ERROR_NOT_SUPPORTED,0,0);ExitProcess(1);}
    if(!(argc==2&&eq(argv[1],L"--status"))){lock=lifecycle_lock(&result);if(!lock)goto done;}
    if(argc==3&&eq(argv[1],L"--install"))result=install(argv[2],TRUE);
    else if(argc==3&&eq(argv[1],L"--ensure-and-feed"))result=ensure_and_feed(argv[2]);
    else if(argc==3&&eq(argv[1],L"--ensure-feedback")){feedback_mode=TRUE;result=ensure_and_feed(argv[2]);}
    else if(argc==2&&eq(argv[1],L"--feedback-feed")){feedback_mode=TRUE;result=feed();}
    else if(argc==3&&eq(argv[1],L"--register"))result=install(argv[2],FALSE);
    else if(argc==2&&eq(argv[1],L"--activate"))result=activate_owned();
    else if(argc==2&&eq(argv[1],L"--uninstall"))result=uninstall();
    else if(argc==2&&eq(argv[1],L"--status")){diagnose();result=0;}
    else if(argc==1||(argc==2&&eq(argv[1],L"--feed")))result=feed();
    else{event("invalid_arguments",FALSE,ERROR_INVALID_PARAMETER,0,0);result=ERROR_INVALID_PARAMETER;}
done:
    if(lock){ReleaseMutex(lock);CloseHandle(lock);}
    if(argv)LocalFree(argv);event("wine_input_command_complete",result==0,result,0,0);ExitProcess(result?1:0);
}
