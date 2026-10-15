// Diagnostics only: no arguments/return values are changed. Interposition is
// confined to our helper process; nothing is injected into Steam or a game.
#include <sys/socket.h>
#include <sys/un.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <stdio.h>
#include <stdbool.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <spawn.h>
#include <unistd.h>
#include <errno.h>
#include <dlfcn.h>
#include <IOKit/hidsystem/IOHIDUserDevice.h>

// Passive diagnostic in our own runtime only. Preserve the original call,
// return value and errno; do not grant permissions or synthesize a device.
static IOHIDUserDeviceRef trace_hid_create(CFAllocatorRef allocator, CFDictionaryRef properties, IOOptionBits options) {
    IOHIDUserDeviceRef result = IOHIDUserDeviceCreateWithProperties(allocator, properties, options);
    int saved_errno = errno;
    const char *enabled = getenv("SKINOS_INPUT_TRACE");
    const char *role = getenv("SKINOS_COMPONENT_ROLE");
    Dl_info info = {0};
    if (enabled && strcmp(enabled,"1")==0 && role && strcmp(role,"runtime")==0 &&
        dladdr(__builtin_return_address(0), &info) && info.dli_fname) {
        const char *name = strrchr(info.dli_fname, '/');
        name = name ? name+1 : info.dli_fname;
        if (strcmp(name,"libsteaminput.dylib")==0) {
            fprintf(stderr,"SKINOS_HID_PROBE {\"event\":\"iohid_virtual_create\",\"created\":%s,\"caller\":\"libsteaminput.dylib\",\"options\":%u}\n",result ? "true" : "false",options);
            fflush(stderr);
        }
    }
    errno = saved_errno;
    return result;
}

static int trace_spawn(pid_t *pid,const char *path,const posix_spawn_file_actions_t *actions,const posix_spawnattr_t *attributes,char *const argv[],char *const env[]) {
    fprintf(stderr,"SKINOS_TRACE posix_spawn path=%s\n",path);fflush(stderr);
    // Keep our adapter in product helper processes, not the launched game.
    char **filtered = NULL;
    char *const *child_env = env;
    if (strcmp(path,"/bin/sh")==0 && env) {
        size_t count=0;while(env[count])count++;
        filtered=calloc(count+1,sizeof(char *));
        if (!filtered) return ENOMEM;
        size_t output=0;
        for(size_t i=0;i<count;i++) {
            if (strncmp(env[i],"DYLD_INSERT_LIBRARIES=",22)==0 && strstr(env[i],"libskinos_trace.dylib")) continue;
            filtered[output++]=env[i];
        }
        child_env=filtered;
    }
    int result=posix_spawn(pid,path,actions,attributes,argv,child_env);
    free(filtered);
    fprintf(stderr,"SKINOS_TRACE posix_spawn result=%d\n",result);fflush(stderr);
    return result;
}
static int trace_execve(const char *path,char *const argv[],char *const env[]) {
    fprintf(stderr,"SKINOS_TRACE execve path=%s\n",path);fflush(stderr);
    return execve(path,argv,env);
}

// Fingerprint-pinned storage adapter for clientdirectories.h. The reviewed
// helper builds its path through this exact format and fixed second string.
// HOME, original files, executable pages and account entitlements are unchanged.
static int isolated_snprintf(char *buffer, size_t size, const char *format, ...) {
    va_list args;
    va_start(args, format);
    const char *root = getenv("SKINOS_RUNTIME_DATA_ROOT");
    if (root && format && strcmp(format, "%s/%s%s") == 0) {
        va_list inspect;
        va_copy(inspect, args);
        (void)va_arg(inspect, const char *);
        const char *component = va_arg(inspect, const char *);
        const char *suffix = va_arg(inspect, const char *);
        va_end(inspect);
        if (component && strcmp(component, "Library/Application Support/Steam") == 0) {
            va_end(args);
            return snprintf(buffer, size, "%s%s", root, suffix ? suffix : "");
        }
    }
    int result = vsnprintf(buffer, size, format, args);
    va_end(args);
    return result;
}

static int trace_bind(int fd, const struct sockaddr *address, socklen_t size) {
    if (address && size >= 2) {
        if (address->sa_family == AF_UNIX) {
            const struct sockaddr_un *value = (const struct sockaddr_un *)address;
            fprintf(stderr, "SKINOS_TRACE bind family=unix path=%.*s\n", (int)sizeof(value->sun_path), value->sun_path);
        } else if (address->sa_family == AF_INET && size >= sizeof(struct sockaddr_in)) {
            const struct sockaddr_in *value = (const struct sockaddr_in *)address;
            char text[INET_ADDRSTRLEN] = {0};
            inet_ntop(AF_INET, &value->sin_addr, text, sizeof(text));
            fprintf(stderr, "SKINOS_TRACE bind family=ipv4 address=%s port=%u\n", text, ntohs(value->sin_port));
        } else {
            fprintf(stderr, "SKINOS_TRACE bind family=%u size=%u\n", address->sa_family, size);
        }
        fflush(stderr);
    }
    return bind(fd, address, size);
}

__attribute__((used, section("__DATA,__interpose")))
static const struct { const void *replacement; const void *original; } replacements[] = {
    { (const void *)trace_hid_create, (const void *)IOHIDUserDeviceCreateWithProperties },
    { (const void *)trace_bind, (const void *)bind },
    { (const void *)isolated_snprintf, (const void *)snprintf },
    { (const void *)trace_spawn, (const void *)posix_spawn },
    { (const void *)trace_execve, (const void *)execve },
};
