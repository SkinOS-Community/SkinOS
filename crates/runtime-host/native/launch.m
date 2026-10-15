// Steam owns authentication, entitlement and the selected launch option. This
// adapter confines its platform handoff to the parent's validated native plan.
#import <AppKit/AppKit.h>
#import <objc/runtime.h>
#include <spawn.h>
#include <unistd.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdatomic.h>
#include <fcntl.h>
#include <limits.h>
#include <sys/time.h>
#include "launch_guard.h"

extern char **environ;
static _Atomic(pid_t) last_game_pid=0;
static _Atomic(pid_t) last_game_group=0;
static _Atomic(uint64_t) last_game_started_microseconds=0;
static NSString *last_game_executable;
pid_t skinos_last_game_pid(void) { return atomic_load(&last_game_pid); }
pid_t skinos_last_game_group(void) { return atomic_load(&last_game_group); }
uint64_t skinos_last_game_started_microseconds(void) { return atomic_load(&last_game_started_microseconds); }
const char *skinos_last_game_executable(void) { return last_game_executable.fileSystemRepresentation; }

@interface SkinOsChildProcess : NSObject
@property(nonatomic) pid_t processIdentifier;
@end
@implementation SkinOsChildProcess
@end

static id rejected(NSError **error,const char *reason) {
    fprintf(stderr,"SKINOS_LAUNCH denied reason=%s\n",reason);fflush(stderr);
    if (error) *error=[NSError errorWithDomain:@"dev.skinos.launch" code:EPERM userInfo:@{NSLocalizedDescriptionKey:@"Launch context was rejected"}];
    return nil;
}
static NSString *configured(const char *name) {
    const char *value=getenv(name);
    return value ? [NSString stringWithUTF8String:value] : nil;
}
static NSString *resolved(NSString *path) {
    if (!path) return nil;
    char canonical[PATH_MAX];
    return realpath(path.fileSystemRepresentation,canonical) ? [NSString stringWithUTF8String:canonical] : nil;
}
static bool contained(NSString *path,NSString *root) {
    return path && root && [path hasPrefix:[root stringByAppendingString:@"/"]];
}
static bool regular_macho(NSString *path) {
    NSFileHandle *file=[NSFileHandle fileHandleForReadingAtPath:path];
    NSData *header=[file readDataOfLength:4];
    [file closeFile];
    if (header.length!=4) return false;
    const unsigned char *b=header.bytes;
    uint32_t magic=((uint32_t)b[0]<<24)|((uint32_t)b[1]<<16)|((uint32_t)b[2]<<8)|b[3];
    return magic==0xfeedface || magic==0xfeedfacf || magic==0xcefaedfe || magic==0xcffaedfe ||
        magic==0xcafebabe || magic==0xbebafeca || magic==0xcafebabf || magic==0xbfbafeca;
}
static bool string_arguments(id value) {
    if (![value isKindOfClass:[NSArray class]] || [value count]>128) return false;
    for (id argument in value) {
        if (![argument isKindOfClass:[NSString class]] || [argument length]>16384 ||
            strlen([argument UTF8String])!=[argument lengthOfBytesUsingEncoding:NSUTF8StringEncoding]) return false;
    }
    return true;
}
// This fixed code restores only our adapter after the protected system shell
// strips DYLD_* on entry. The published script remains the launcher, with its
// original $0, positional arguments and cwd. No path is interpolated as code.
static NSString *const scriptBootstrap=@"export DYLD_INSERT_LIBRARIES=\"$SKINOS_ADAPTER_PATH\"; . \"$0\"";

static id native_launch(id object,SEL selector,NSURL *url,NSWorkspaceLaunchOptions options,NSDictionary *configuration,NSError **error) {
    (void)object;(void)selector;(void)options;
    const char *expected=getenv("SKINOS_LAUNCH_APP_ID");
    NSString *selected=configured("SKINOS_GAME_BUNDLE");
    if (!url.isFileURL || !selected || !expected) return rejected(error,"non_file_or_unconfigured_target");
    NSString *target=resolved(url.path);
    NSString *allowed=resolved(selected);
    NSString *root=resolved(configured("SKINOS_GAME_INSTALL_ROOT"));
    if (!target || ![target isEqualToString:allowed]) return rejected(error,"different_application");
    if (!contained(target,root)) return rejected(error,"target_outside_installation");
    id provided=configuration[NSWorkspaceLaunchConfigurationEnvironment];
    if (![provided isKindOfClass:[NSDictionary class]]) return rejected(error,"missing_steam_environment");
    for (id key in provided) if (![key isKindOfClass:[NSString class]] || ![provided[key] isKindOfClass:[NSString class]]) return rejected(error,"invalid_steam_environment");
    NSString *appid=provided[@"SteamAppId"];
    NSString *gameid=provided[@"SteamGameId"];
    bool valid=skinos_launch_context_valid(expected,appid.UTF8String,gameid.UTF8String);
    fprintf(stderr,"SKINOS_LAUNCH context appid_matches=%d gameid_matches=%d client_launch_present=%d valid=%d\n",appid && strcmp(expected,appid.UTF8String)==0,gameid && strcmp(expected,gameid.UTF8String)==0,provided[@"SteamClientLaunch"]!=nil,valid);fflush(stderr);
    if (!valid) return rejected(error,"missing_or_wrong_steam_context");
    NSString *kind=configured("SKINOS_NATIVE_TARGET_KIND");
    NSString *executable=nil;
    NSMutableArray<NSString *> *prefix=[NSMutableArray array];
    if ([kind isEqualToString:@"bundle"]) {
        executable=resolved([NSBundle bundleWithPath:target].executablePath);
        if (!contained(executable,target) || !regular_macho(executable)) return rejected(error,"invalid_bundle_executable");
    } else if ([kind isEqualToString:@"binary"]) {
        executable=target;
        if (!regular_macho(executable)) return rejected(error,"invalid_native_binary");
    } else if ([kind isEqualToString:@"script"]) {
        executable=configured("SKINOS_NATIVE_INTERPRETER");
        if (!([executable isEqualToString:@"/bin/sh"] || [executable isEqualToString:@"/bin/bash"])) return rejected(error,"unsupported_script_interpreter");
        NSString *option=configured("SKINOS_NATIVE_INTERPRETER_ARGUMENT");
        if (option) {
            NSCharacterSet *flags=[NSCharacterSet characterSetWithCharactersInString:@"eufvx"];
            if (![option hasPrefix:@"-"] || option.length<2 || [[option substringFromIndex:1] rangeOfCharacterFromSet:flags.invertedSet].location!=NSNotFound) return rejected(error,"unsupported_script_option");
            [prefix addObject:option];
        }
        [prefix addObjectsFromArray:@[@"-c",scriptBootstrap,target]];
    } else return rejected(error,"unknown_native_target_kind");
    if (![resolved(configured("SKINOS_NATIVE_EXECUTABLE")) isEqualToString:([kind isEqualToString:@"script"] ? target : executable)]) return rejected(error,"selected_executable_changed");
    NSString *cwd=resolved(configured("SKINOS_GAME_WORKING_DIR"));
    BOOL cwdDirectory=NO;
    if (!cwd || !([cwd isEqualToString:root] || contained(cwd,root)) || ![[NSFileManager defaultManager] fileExistsAtPath:cwd isDirectory:&cwdDirectory] || !cwdDirectory) return rejected(error,"invalid_working_directory");
    const char *adapter=getenv("SKINOS_ADAPTER_PATH");
    const char *service=getenv("SKINOS_IPC_SERVICE_NAME");
    if (!adapter || !service || strncmp(service,"dev.skinos.ipctool.",19)!=0) return rejected(error,"owned_broker_missing");
    const char *lockValue=getenv("SKINOS_PROFILE_LOCK_FD");
    char *lockEnd=NULL;
    errno=0;
    long lockFD=lockValue ? strtol(lockValue,&lockEnd,10) : -1;
    if (!lockValue || lockEnd==lockValue || *lockEnd || errno || lockFD<=STDERR_FILENO || lockFD>INT_MAX || fcntl((int)lockFD,F_GETFD)<0) return rejected(error,"profile_lock_missing");
    NSMutableDictionary<NSString *,NSString *> *environment=[NSMutableDictionary dictionary];
    for (char **entry=environ;entry && *entry;entry++) {
        NSString *line=[NSString stringWithUTF8String:*entry];
        if (!line) continue;
        NSRange equal=[line rangeOfString:@"="];
        if (equal.location!=NSNotFound) environment[[line substringToIndex:equal.location]]=[line substringFromIndex:equal.location+1];
    }
    [environment addEntriesFromDictionary:provided];
    [environment removeObjectForKey:@"SKINOS_PROFILE_LOCK_FD"];
    environment[@"DYLD_INSERT_LIBRARIES"]=[NSString stringWithUTF8String:adapter];
    environment[@"SKINOS_ADAPTER_PATH"]=[NSString stringWithUTF8String:adapter];
    environment[@"SKINOS_LAUNCH_APP_ID"]=[NSString stringWithUTF8String:expected];
    environment[@"SKINOS_COMPONENT_ROLE"]=@"game";
    environment[@"SKINOS_IPC_SERVICE_NAME"]=[NSString stringWithUTF8String:service];
    id arguments=configuration[NSWorkspaceLaunchConfigurationArguments] ?: @[];
    const char *requestedArguments=getenv("SKINOS_GAME_ARGUMENTS_JSON");
    if (requestedArguments) {
        NSData *data=[[NSString stringWithUTF8String:requestedArguments] dataUsingEncoding:NSUTF8StringEncoding];
        arguments=[NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
    }
    if (!string_arguments(arguments)) return rejected(error,"invalid_arguments");
    [prefix addObjectsFromArray:arguments];
    char **argv=calloc(prefix.count+2,sizeof(char *));
    char **envp=calloc(environment.count+1,sizeof(char *));
    if (!argv || !envp) { free(argv);free(envp);return rejected(error,"allocation_failure"); }
    argv[0]=strdup(executable.fileSystemRepresentation);
    for (NSUInteger i=0;i<prefix.count;i++) argv[i+1]=strdup(prefix[i].UTF8String);
    NSUInteger index=0;
    for (NSString *key in environment) envp[index++]=strdup([[NSString stringWithFormat:@"%@=%@",key,environment[key]] UTF8String]);
    posix_spawn_file_actions_t actions;
    int setup=posix_spawn_file_actions_init(&actions);
    bool initialized=setup==0;
    if (!setup) setup=posix_spawn_file_actions_addclose(&actions,(int)lockFD);
    NSString *state=configured("SKINOS_RUNTIME_STATE_ROOT");
    if (!state) setup=EINVAL;
    if (!setup) setup=posix_spawn_file_actions_addopen(&actions,STDIN_FILENO,"/dev/null",O_RDONLY,0);
    if (!setup) setup=posix_spawn_file_actions_addopen(&actions,STDOUT_FILENO,[state stringByAppendingPathComponent:@"game.stdout.log"].fileSystemRepresentation,O_WRONLY|O_CREAT|O_APPEND,0600);
    if (!setup) setup=posix_spawn_file_actions_addopen(&actions,STDERR_FILENO,[state stringByAppendingPathComponent:@"game.stderr.log"].fileSystemRepresentation,O_WRONLY|O_CREAT|O_APPEND,0600);
    if (!setup) setup=posix_spawn_file_actions_addchdir_np(&actions,cwd.fileSystemRepresentation);
    posix_spawnattr_t attributes;
    bool attributesInitialized=false;
    bool script=[kind isEqualToString:@"script"];
    if (!setup && script) {
        setup=posix_spawnattr_init(&attributes);
        attributesInitialized=setup==0;
        if (!setup) setup=posix_spawnattr_setpgroup(&attributes,0);
        if (!setup) setup=posix_spawnattr_setflags(&attributes,POSIX_SPAWN_SETPGROUP);
    }
    struct timeval started={0};
    if (!setup && gettimeofday(&started,NULL)!=0) setup=errno ? errno : EINVAL;
    uint64_t startedMicroseconds=(uint64_t)started.tv_sec*1000000ULL+(uint64_t)started.tv_usec;
    pid_t pid=0;
    int result=setup ? setup : posix_spawn(&pid,executable.fileSystemRepresentation,&actions,script ? &attributes : NULL,argv,envp);
    if (attributesInitialized) posix_spawnattr_destroy(&attributes);
    if (initialized) posix_spawn_file_actions_destroy(&actions);
    for (NSUInteger i=0;argv[i];i++) free(argv[i]);
    for (NSUInteger i=0;envp[i];i++) free(envp[i]);
    free(argv);free(envp);
    if (result!=0) return rejected(error,setup ? "native_spawn_setup_failed" : "native_spawn_failed");
    last_game_executable=executable;
    atomic_store(&last_game_started_microseconds,startedMicroseconds);
    atomic_store(&last_game_group,script ? pid : 0);
    atomic_store(&last_game_pid,pid);
    fprintf(stderr,"SKINOS_LAUNCH spawned pid=%d appid=%s kind=%s\n",pid,expected,kind.UTF8String);fflush(stderr);
    SkinOsChildProcess *process=[SkinOsChildProcess new];
    process.processIdentifier=pid;
    return process;
}
static id native_open_url(id object,SEL selector,NSURL *url,NSWorkspaceLaunchOptions options,NSDictionary *configuration,NSError **error) {
    return native_launch(object,selector,url,options,configuration,error);
}
static id native_open_urls(id object,SEL selector,NSArray *urls,NSURL *application,NSWorkspaceLaunchOptions options,NSDictionary *configuration,NSError **error) {
    if (!urls.count) return native_launch(object,selector,application,options,configuration,error);
    // LaunchServices may route a script to its registered text editor. Accept
    // exactly the metadata-selected file, and execute that file's script plan.
    // The default application is never launched and cannot choose another game.
    NSString *kind=configured("SKINOS_NATIVE_TARGET_KIND");
    if (urls.count!=1 || !([kind isEqualToString:@"script"] || [kind isEqualToString:@"binary"]) || ![urls[0] isKindOfClass:[NSURL class]]) return rejected(error,"document_or_url_launch_not_supported");
    return native_launch(object,selector,urls[0],options,configuration,error);
}
__attribute__((constructor)) static void initialize_platform_adapter(void) {
    const char *role=getenv("SKINOS_COMPONENT_ROLE");
    if (role && strcmp(role,"game")==0) {
        if (!skinos_launch_context_valid(getenv("SKINOS_LAUNCH_APP_ID"),getenv("SteamAppId"),getenv("SteamGameId"))) {
            fprintf(stderr,"SKINOS_LAUNCH child context invalid; exiting before game main\n");
            _exit(125);
        }
    }
    if (!role || strcmp(role,"runtime")!=0) return;
    Class workspace=objc_getClass("NSWorkspace");
    Method launch=class_getInstanceMethod(workspace,sel_registerName("launchApplicationAtURL:options:configuration:error:"));
    Method open=class_getInstanceMethod(workspace,sel_registerName("openURL:options:configuration:error:"));
    Method opens=class_getInstanceMethod(workspace,sel_registerName("openURLs:withApplicationAtURL:options:configuration:error:"));
    if (!launch || !open || !opens) { fprintf(stderr,"SKINOS_LAUNCH required platform methods missing\n");_exit(126); }
    method_setImplementation(launch,(IMP)native_launch);
    method_setImplementation(open,(IMP)native_open_url);
    method_setImplementation(opens,(IMP)native_open_urls);
}
