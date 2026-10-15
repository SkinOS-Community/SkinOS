// Private discovery routing only. The genuine Valve broker and all Steamworks
// authentication/entitlement messages remain unchanged.
#include <servers/bootstrap.h>
#include <mach/mach.h>
#include <stdlib.h>
#include <string.h>
#include <stddef.h>
#include <stdio.h>
#include <pthread.h>
#include <unistd.h>
#include <signal.h>
#include <errno.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <spawn.h>
#include <limits.h>
#include <time.h>
#include <launch.h>
#include <mach-o/dyld.h>

#define SKINOS_IPC_PREFIX "dev.skinos.ipctool."
#define STEAM_IPC_SERVICE "com.valvesoftware.steam.ipctool"

static const char *configured_service(void) {
    const char *name = getenv("SKINOS_IPC_SERVICE_NAME");
    const size_t prefix_len = sizeof(SKINOS_IPC_PREFIX) - 1;
    if (!name || strncmp(name, SKINOS_IPC_PREFIX, prefix_len) != 0) return NULL;
    const size_t len = strnlen(name, BOOTSTRAP_MAX_NAME_LEN);
    if (len <= prefix_len || len >= BOOTSTRAP_MAX_NAME_LEN) return NULL;
    for (size_t i = prefix_len; i < len; ++i) {
        const unsigned char c = (unsigned char)name[i];
        if (!((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '-')) return NULL;
    }
    return name;
}

// Pure preflight: no Mach lookup, registration, or launchd message is performed.
int skinos_ipc_is_configured(void) { return configured_service() != NULL; }

// 1: remapped private endpoint; 0: unrelated service; -1: forbidden Valve lookup.
// Exported for deterministic checks without ever contacting the original broker.
int skinos_ipc_route(const char *requested, char *output, size_t capacity) {
    if (!requested) return -1;
    if (strcmp(requested, STEAM_IPC_SERVICE) == 0) {
        const char *owned = configured_service();
        if (!owned || !output || strlen(owned) + 1 > capacity) return -1;
        memcpy(output, owned, strlen(owned) + 1);
        return 1;
    }
    if (strncmp(requested, "com.valvesoftware.", sizeof("com.valvesoftware.") - 1) == 0 ||
        strncmp(requested, "com.valve.", sizeof("com.valve.") - 1) == 0) return -1;
    return 0;
}

static int is_original_valve_name(const char *name) {
    return name && (strncmp(name,"com.valvesoftware.",sizeof("com.valvesoftware.")-1)==0 ||
                    strncmp(name,"com.valve.",sizeof("com.valve.")-1)==0);
}
static int denies_original_job_mutation(launch_data_t request) {
    if (!request || launch_data_get_type(request)!=LAUNCH_DATA_DICTIONARY) return 0;
    const char *keys[]={"StopJob","StartJob","RemoveJob"};
    for(size_t i=0;i<sizeof(keys)/sizeof(keys[0]);++i) {
        launch_data_t value=launch_data_dict_lookup(request,keys[i]);
        if(value && launch_data_get_type(value)==LAUNCH_DATA_STRING &&
           is_original_valve_name(launch_data_get_string(value))) return 1;
    }
    return 0;
}
#ifdef SKINOS_IPC_SELF_TEST
static int original_launch_calls;
static launch_data_t fake_launch_msg(launch_data_t request) {
    (void)request;++original_launch_calls;return launch_data_alloc(LAUNCH_DATA_ERRNO);
}
#define REAL_LAUNCH_MSG fake_launch_msg
#else
#define REAL_LAUNCH_MSG launch_msg
#endif
static launch_data_t skinos_launch_msg(launch_data_t request) {
    if(denies_original_job_mutation(request)) {
        fputs("SKINOS_IPC_GUARD denied_original_launch_job\n",stderr);
        errno=EPERM;
        return NULL;
    }
    return REAL_LAUNCH_MSG(request);
}

#ifdef SKINOS_IPC_SELF_TEST
static int original_calls;
static char last_service[BOOTSTRAP_MAX_NAME_LEN];
static kern_return_t fake_original(mach_port_t bp, const name_t name, mach_port_t *port) {
    (void)bp;
    ++original_calls;
    strlcpy(last_service, name, sizeof(last_service));
    if (port) *port = 7;
    return KERN_SUCCESS;
}
#define REAL_LOOKUP fake_original
#define REAL_CHECKIN fake_original
#else
#define REAL_LOOKUP bootstrap_look_up
#define REAL_CHECKIN bootstrap_check_in
#endif

static kern_return_t skinos_bootstrap_look_up(mach_port_t bp, const name_t requested, mach_port_t *port) {
    char owned[BOOTSTRAP_MAX_NAME_LEN];
    const int route = skinos_ipc_route(requested, owned, sizeof(owned));
    if (route < 0) {
        if (port) *port = MACH_PORT_NULL;
        return BOOTSTRAP_UNKNOWN_SERVICE;
    }
    return REAL_LOOKUP(bp, route == 1 ? owned : requested, port);
}

static kern_return_t skinos_bootstrap_check_in(mach_port_t bp, const name_t requested, mach_port_t *port) {
    char owned[BOOTSTRAP_MAX_NAME_LEN];
    const int route = skinos_ipc_route(requested, owned, sizeof(owned));
    if (route < 0) {
        if (port) *port = MACH_PORT_NULL;
        return BOOTSTRAP_UNKNOWN_SERVICE;
    }
    return REAL_CHECKIN(bp, route == 1 ? owned : requested, port);
}

#ifndef SKINOS_IPC_SELF_TEST
struct DiscoveryMessage {
    mach_msg_header_t header;
    unsigned int opcode;
    int pid;
    char path[512];
    mach_msg_max_trailer_t trailer;
};
// Actual broker metadata, never SteamAPI/authentication results.
int skinos_ipc_query_process(int *pid,char *path,size_t capacity) {
    if(pid)*pid=0;if(path && capacity)path[0]=0;
    const char *service=configured_service();
    if(!service || !pid || !path || capacity==0)return -1;
    mach_port_t server=MACH_PORT_NULL,reply=MACH_PORT_NULL;
    if(bootstrap_look_up(bootstrap_port,service,&server)!=KERN_SUCCESS)return -1;
    if(mach_port_allocate(mach_task_self(),MACH_PORT_RIGHT_RECEIVE,&reply)!=KERN_SUCCESS) {
        mach_port_deallocate(mach_task_self(),server);return -1;
    }
    struct DiscoveryMessage message={0};
    message.header.msgh_bits=MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND,MACH_MSG_TYPE_MAKE_SEND);
    message.header.msgh_size=32;message.header.msgh_remote_port=server;
    message.header.msgh_local_port=reply;message.header.msgh_id=104;message.opcode=14;
    const kern_return_t result=mach_msg(&message.header,MACH_SEND_MSG|MACH_RCV_MSG|MACH_SEND_TIMEOUT|MACH_RCV_TIMEOUT,
        32,sizeof(message),reply,1000,MACH_PORT_NULL);
    mach_port_deallocate(mach_task_self(),server);
    mach_port_mod_refs(mach_task_self(),reply,MACH_PORT_RIGHT_RECEIVE,-1);
    if(result!=MACH_MSG_SUCCESS || message.header.msgh_id!=104 || message.opcode!=14 || message.header.msgh_size<32)return -1;
    if(message.pid<=0 || message.path[0]==0)return 0;
    const size_t length=strnlen(message.path,sizeof(message.path));
    if(length==sizeof(message.path) || length+1>capacity)return -1;
    *pid=message.pid;memcpy(path,message.path,length+1);return 1;
}
int skinos_ipc_register_current_process(void) {
    const char *role=getenv("SKINOS_COMPONENT_ROLE");
    const char *service=configured_service();const char *state=getenv("SKINOS_RUNTIME_STATE_ROOT");
    if(!role || strcmp(role,"runtime")!=0 || !service || !state)return -1;
    char executable_raw[PATH_MAX],executable[PATH_MAX],state_path[PATH_MAX];
    uint32_t capacity=sizeof(executable_raw);
    if(_NSGetExecutablePath(executable_raw,&capacity)!=0 || !realpath(executable_raw,executable) || !realpath(state,state_path))return -1;
    char required_parent[PATH_MAX];
    const int length=snprintf(required_parent,sizeof(required_parent),"%s/runtime/",state_path);
    if(length<0 || (size_t)length>=sizeof(required_parent) || strncmp(executable,required_parent,(size_t)length)!=0 || strlen(executable)>=512)return -1;
    mach_port_t server=MACH_PORT_NULL;
    if(bootstrap_look_up(bootstrap_port,service,&server)!=KERN_SUCCESS)return -1;
    struct DiscoveryMessage message={0};
    message.header.msgh_bits=MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND,0);
    message.header.msgh_size=544;message.header.msgh_remote_port=server;message.header.msgh_id=104;
    message.opcode=13;message.pid=getpid();strlcpy(message.path,executable,sizeof(message.path));
    const kern_return_t result=mach_msg(&message.header,MACH_SEND_MSG|MACH_SEND_TIMEOUT,544,0,MACH_PORT_NULL,1000,MACH_PORT_NULL);
    mach_port_deallocate(mach_task_self(),server);
    if(result!=MACH_MSG_SUCCESS)return -1;
    int registered_pid=0;char registered_path[512];
    const int queried=skinos_ipc_query_process(&registered_pid,registered_path,sizeof(registered_path));
    return queried==1 && registered_pid==getpid() && strcmp(registered_path,executable)==0 ? 1 : 0;
}
#endif

#ifndef SKINOS_IPC_SELF_TEST
// Only the copied broker role owns this watcher. It never signals the runtime,
// game, or any existing Valve process: its sole mutation is bootout of its exact
// nonce-labelled launchd job after the recorded owner dies.
static int valid_broker_label(const char *label) {
    static const char prefix[] = "dev.skinos.ipc.";
    const char *service = configured_service();
    return service && label && strncmp(label, prefix, sizeof(prefix)-1) == 0 &&
        strcmp(label + sizeof(prefix)-1, service + sizeof(SKINOS_IPC_PREFIX)-1) == 0;
}

static pid_t read_owner_pid(const char *path) {
    int fd = open(path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) return 0;
    struct stat info;
    if (fstat(fd, &info) != 0 || !S_ISREG(info.st_mode) || info.st_uid != geteuid()) {
        close(fd); return 0;
    }
    char text[64];
    ssize_t count = read(fd, text, sizeof(text)-1);
    close(fd);
    if (count <= 0 || count == (ssize_t)sizeof(text)-1) return 0;
    text[count] = 0;
    errno = 0;
    char *end = NULL;
    long pid = strtol(text, &end, 10);
    if (errno || end == text || pid <= 1 || pid > INT_MAX) return 0;
    while (*end == ' ' || *end == '\n' || *end == '\r' || *end == '\t') ++end;
    return *end == 0 ? (pid_t)pid : 0;
}

static double monotonic_seconds(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1000000000.0;
}

static void *watch_owner(void *unused) {
    (void)unused;
    const char *label = getenv("SKINOS_BROKER_LABEL");
    const char *owner_file = getenv("SKINOS_BROKER_OWNER_FILE");
    const char *parent_text = getenv("SKINOS_BROKER_PARENT_PID");
    const long parent_pid = parent_text ? strtol(parent_text, NULL, 10) : 0;
    const double started = monotonic_seconds();
    double dead_since = 0;
    int worker_seen = 0;
    for (;;) {
        const pid_t owner = read_owner_pid(owner_file);
        const int alive = owner > 1 && (kill(owner, 0) == 0 || errno == EPERM);
        if (alive) {
            dead_since = 0;
            if (owner != parent_pid) worker_seen = 1;
        } else {
            const double now = monotonic_seconds();
            if ((worker_seen || now - started >= 10.0) && dead_since == 0) dead_since = now;
            if (dead_since != 0 && now - dead_since >= 3.0) {
                if (!valid_broker_label(label)) return NULL;
                char target[256];
                snprintf(target, sizeof(target), "gui/%u/%s", (unsigned)geteuid(), label);
                char *const args[] = { "/bin/launchctl", "bootout", target, NULL };
                // No adapter/environment is inherited by the cleanup utility.
                char *const environment[] = { NULL };
                pid_t child = 0;
                const int spawned = posix_spawn(&child, "/bin/launchctl", NULL, NULL, args, environment);
                if (spawned == 0) {
                    int status = 0;
                    while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
                }
                dead_since = monotonic_seconds();
            }
        }
        struct timespec pause = { .tv_sec = 0, .tv_nsec = 250000000 };
        nanosleep(&pause, NULL);
    }
}

__attribute__((constructor))
static void start_broker_watchdog(void) {
    const char *role = getenv("SKINOS_COMPONENT_ROLE");
    if (!role || strcmp(role, "broker") != 0) return;
    const char *label = getenv("SKINOS_BROKER_LABEL");
    const char *owner = getenv("SKINOS_BROKER_OWNER_FILE");
    if (!valid_broker_label(label) || !owner || owner[0] != '/') {
        fputs("SKINOS_BROKER_WATCHDOG invalid private lifecycle configuration\n", stderr);
        _exit(70);
    }
    pthread_t thread;
    if (pthread_create(&thread, NULL, watch_owner, NULL) != 0) {
        fputs("SKINOS_BROKER_WATCHDOG cannot start owner watcher\n", stderr);
        _exit(70);
    }
    pthread_detach(thread);
}
#endif

#ifndef SKINOS_IPC_SELF_TEST
// Temporary scoped diagnostics for the discovery protocol only. Never inspect
// engine/auth traffic: these records are solely registered process ID and path.
static unsigned int discovery_opcode(const mach_msg_header_t *message, mach_msg_size_t available) {
    const char *role=getenv("SKINOS_COMPONENT_ROLE");
    if (!role || strcmp(role,"broker")!=0 || !configured_service() || !message ||
        available<32 || message->msgh_size<32 || message->msgh_size>available || message->msgh_id!=104) return 0;
    unsigned int opcode=0;
    memcpy(&opcode,(const char *)message+24,sizeof(opcode));
    return opcode==13 || opcode==14 ? opcode : 0;
}
static void trace_discovery(const mach_msg_header_t *message,mach_msg_size_t available,const char *direction) {
    const unsigned int opcode=discovery_opcode(message,available);
    if (!opcode) return;
    const int saved_errno=errno;
    int pid=0;memcpy(&pid,(const char *)message+28,sizeof(pid));
    char path[513]={0};
    size_t count=message->msgh_size-32;if(count>512)count=512;
    if(count)memcpy(path,(const char *)message+32,count);
    path[512]=0;
    for(size_t i=0;i<sizeof(path)&&path[i];++i) if((unsigned char)path[i]<' ')path[i]='?';
    const int alive=pid>1 && (kill(pid,0)==0 || errno==EPERM);
    errno=0;const int readable=path[0] ? access(path,R_OK)==0 : 0;const int path_error=errno;
    fprintf(stderr,"SKINOS_IPC_METADATA direction=%s op=%u pid=%d alive=%d path_readable=%d path_errno=%d path=%s\n",direction,opcode,pid,alive,readable,path_error,path);
    fflush(stderr);errno=saved_errno;
}
static mach_msg_return_t skinos_mach_msg(mach_msg_header_t *message,mach_msg_option_t options,mach_msg_size_t send_size,mach_msg_size_t receive_size,mach_port_name_t receive_name,mach_msg_timeout_t timeout,mach_port_name_t notify) {
    const unsigned int sent=(options&MACH_SEND_MSG)?discovery_opcode(message,send_size):0;
    if(sent)trace_discovery(message,send_size,"send");
    const mach_msg_return_t result=mach_msg(message,options,send_size,receive_size,receive_name,timeout,notify);
    if(result==MACH_MSG_SUCCESS && (options&MACH_RCV_MSG))trace_discovery(message,receive_size,"receive");
    if(result!=MACH_MSG_SUCCESS && sent){fprintf(stderr,"SKINOS_IPC_METADATA send_failed op=%u result=%d\n",sent,result);fflush(stderr);}
    return result;
}
#endif

#ifndef SKINOS_IPC_SELF_TEST
__attribute__((used, section("__DATA,__interpose")))
static const struct { const void *replacement; const void *original; } ipc_replacements[] = {
    { (const void *)skinos_bootstrap_look_up, (const void *)bootstrap_look_up },
    { (const void *)skinos_bootstrap_check_in, (const void *)bootstrap_check_in },
    { (const void *)skinos_mach_msg, (const void *)mach_msg },
    { (const void *)skinos_launch_msg, (const void *)launch_msg },
};
#else
#include <assert.h>
#include <stdio.h>
int main(void) {
    mach_port_t port = 99;
    unsetenv("SKINOS_IPC_SERVICE_NAME");
    assert(skinos_ipc_is_configured() == 0);
    assert(skinos_bootstrap_look_up(0, STEAM_IPC_SERVICE, &port) == BOOTSTRAP_UNKNOWN_SERVICE);
    assert(port == MACH_PORT_NULL && original_calls == 0);
    assert(skinos_bootstrap_check_in(0, STEAM_IPC_SERVICE, &port) == BOOTSTRAP_UNKNOWN_SERVICE);
    assert(original_calls == 0);
    setenv("SKINOS_IPC_SERVICE_NAME", STEAM_IPC_SERVICE, 1);
    assert(skinos_ipc_is_configured() == 0);
    assert(skinos_bootstrap_look_up(0, STEAM_IPC_SERVICE, &port) == BOOTSTRAP_UNKNOWN_SERVICE);
    assert(original_calls == 0);
    setenv("SKINOS_IPC_SERVICE_NAME", "dev.skinos.ipctool.test-123", 1);
    assert(skinos_ipc_is_configured() == 1);
    assert(skinos_bootstrap_look_up(0, "com.valve.steam.other", &port) == BOOTSTRAP_UNKNOWN_SERVICE);
    assert(skinos_bootstrap_look_up(0, "com.valvesoftware.steam.other", &port) == BOOTSTRAP_UNKNOWN_SERVICE);
    assert(skinos_bootstrap_check_in(0, "com.valvesoftware.steam.other", &port) == BOOTSTRAP_UNKNOWN_SERVICE);
    assert(original_calls == 0);
    assert(skinos_bootstrap_look_up(0, STEAM_IPC_SERVICE, &port) == KERN_SUCCESS);
    assert(strcmp(last_service, "dev.skinos.ipctool.test-123") == 0 && original_calls == 1);
    assert(skinos_bootstrap_check_in(0, STEAM_IPC_SERVICE, &port) == KERN_SUCCESS);
    assert(strcmp(last_service, "dev.skinos.ipctool.test-123") == 0 && original_calls == 2);
    assert(skinos_bootstrap_look_up(0, "dev.skinos.unrelated-test", &port) == KERN_SUCCESS);
    assert(strcmp(last_service, "dev.skinos.unrelated-test") == 0);
    const char *operations[]={"StopJob","StartJob","RemoveJob"};
    for(size_t i=0;i<sizeof(operations)/sizeof(operations[0]);++i) {
        launch_data_t request=launch_data_alloc(LAUNCH_DATA_DICTIONARY);
        launch_data_dict_insert(request,launch_data_new_string(STEAM_IPC_SERVICE),operations[i]);
        launch_data_t result=skinos_launch_msg(request);
        assert(result==NULL && errno==EPERM);
        assert(original_launch_calls==0);launch_data_free(request);
    }
    launch_data_t request=launch_data_alloc(LAUNCH_DATA_DICTIONARY);
    launch_data_dict_insert(request,launch_data_new_string("dev.skinos.ipc.test-123"),"StopJob");
    launch_data_t result=skinos_launch_msg(request);assert(original_launch_calls==1);
    launch_data_free(result);launch_data_free(request);
    puts("PASS: private IPC routing and original Valve launch-job mutation denial; no original service/job calls");
    return 0;
}
#endif
