// Mac platform launch adapter: real Steam engine still owns authentication,
// ownership, launch metadata and process lifetime. Only NSWorkspace's remote
// LaunchServices handoff is replaced with a direct, confined child spawn so the
// child retains the intended environment and owned IPC broker namespace.
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
#include "launch_guard.h"

extern char **environ;
static _Atomic(pid_t) last_game_pid=0;
static NSString *last_game_executable;
pid_t skinos_last_game_pid(void) { return atomic_load(&last_game_pid); }
const char *skinos_last_game_executable(void) { return last_game_executable.fileSystemRepresentation; }

// Steam's reviewed Mac platform wrapper reads only processIdentifier from its
// successful launch result. This object represents the actual posix_spawn PID.
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

static id native_launch(id object,SEL selector,NSURL *url,NSWorkspaceLaunchOptions options,NSDictionary *configuration,NSError **error) {
    (void)object;(void)selector;(void)options;
    const char *expected=getenv("SKINOS_LAUNCH_APP_ID");
    const char *selected=getenv("SKINOS_GAME_BUNDLE");
    if (!url.isFileURL || !selected || !expected) return rejected(error,"non_file_or_unconfigured_target");
    NSString *target=url.path.stringByResolvingSymlinksInPath;
    NSString *allowed=[[NSString stringWithUTF8String:selected] stringByResolvingSymlinksInPath];
    if (![target isEqualToString:allowed]) return rejected(error,"different_application");
    NSDictionary *provided=configuration[NSWorkspaceLaunchConfigurationEnvironment];
    NSString *appid=provided[@"SteamAppId"];
    NSString *gameid=provided[@"SteamGameId"];
    NSString *launched=provided[@"SteamClientLaunch"];
    bool valid=skinos_launch_context_valid(expected,appid.UTF8String,gameid.UTF8String);
    fprintf(stderr,"SKINOS_LAUNCH context appid_matches=%d gameid_matches=%d client_launch_present=%d valid=%d\n",appid && strcmp(expected,appid.UTF8String)==0,gameid && strcmp(expected,gameid.UTF8String)==0,launched!=nil,valid);fflush(stderr);
    if (!valid) return rejected(error,"missing_or_wrong_steam_context");
    NSBundle *bundle=[NSBundle bundleWithPath:target];
    NSString *executable=bundle.executablePath;
    if (!executable || ![executable.stringByResolvingSymlinksInPath hasPrefix:[allowed stringByAppendingString:@"/"]]) return rejected(error,"invalid_bundle_executable");
    const char *adapter=getenv("SKINOS_ADAPTER_PATH");
    const char *service=getenv("SKINOS_IPC_SERVICE_NAME");
    if (!adapter || !service || strncmp(service,"dev.skinos.ipctool.",19)!=0) return rejected(error,"owned_broker_missing");
    // The parent worker is the sole lock owner. Valve/Wine may clear CLOEXEC,
    // so require an explicit close action for this descriptor in the game.
    const char *lockValue=getenv("SKINOS_PROFILE_LOCK_FD");
    char *lockEnd=NULL;
    errno=0;
    long lockFD=lockValue ? strtol(lockValue,&lockEnd,10) : -1;
    if (!lockValue || lockEnd==lockValue || *lockEnd || errno || lockFD<=STDERR_FILENO || lockFD>INT_MAX || fcntl((int)lockFD,F_GETFD)<0) return rejected(error,"profile_lock_missing");
    NSMutableDictionary<NSString *,NSString *> *environment=[NSMutableDictionary dictionary];
    for (char **entry=environ;entry && *entry;entry++) {
        NSString *line=[NSString stringWithUTF8String:*entry];
        NSRange equal=[line rangeOfString:@"="];
        if (equal.location!=NSNotFound) environment[[line substringToIndex:equal.location]]=[line substringFromIndex:equal.location+1];
    }
    [environment addEntriesFromDictionary:provided];
    [environment removeObjectForKey:@"SKINOS_PROFILE_LOCK_FD"];
    // Deliberately inject only our documented IPC/directory adapter into this
    // selected game. The game's Steam SDK and all authorization stay original.
    environment[@"DYLD_INSERT_LIBRARIES"]=[NSString stringWithUTF8String:adapter];
    environment[@"SKINOS_COMPONENT_ROLE"]=@"game";
    environment[@"SKINOS_IPC_SERVICE_NAME"]=[NSString stringWithUTF8String:service];
    NSArray<NSString *> *arguments=configuration[NSWorkspaceLaunchConfigurationArguments] ?: @[];
    const char *requestedArguments=getenv("SKINOS_GAME_ARGUMENTS_JSON");
    if (requestedArguments) {
        NSData *data=[[NSString stringWithUTF8String:requestedArguments] dataUsingEncoding:NSUTF8StringEncoding];
        id parsed=[NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
        if (![parsed isKindOfClass:[NSArray class]]) return rejected(error,"invalid_arguments");
        for (id argument in parsed) if (![argument isKindOfClass:[NSString class]]) return rejected(error,"invalid_argument_type");
        arguments=parsed;
    }
    if (arguments.count>128) return rejected(error,"argument_limit");
    char **argv=calloc(arguments.count+2,sizeof(char *));
    char **envp=calloc(environment.count+1,sizeof(char *));
    if (!argv || !envp) { free(argv);free(envp);return rejected(error,"allocation_failure"); }
    argv[0]=strdup(executable.fileSystemRepresentation);
    for (NSUInteger i=0;i<arguments.count;i++) argv[i+1]=strdup(arguments[i].UTF8String);
    NSUInteger index=0;
    for (NSString *key in environment) envp[index++]=strdup([[NSString stringWithFormat:@"%@=%@",key,environment[key]] UTF8String]);
    posix_spawn_file_actions_t actions;
    int setup=posix_spawn_file_actions_init(&actions);
    bool initialized=setup==0;
    if (initialized) setup=posix_spawn_file_actions_addclose(&actions,(int)lockFD);
    if (setup) {
        if (initialized) posix_spawn_file_actions_destroy(&actions);
        for (NSUInteger i=0;argv[i];i++) free(argv[i]);
        for (NSUInteger i=0;envp[i];i++) free(envp[i]);
        free(argv);free(envp);
        return rejected(error,"profile_lock_close_failed");
    }
    const char *state=getenv("SKINOS_RUNTIME_STATE_ROOT");
    if (!state) {posix_spawn_file_actions_destroy(&actions);return rejected(error,"runtime_state_missing");}
    NSString *stdoutPath=[[NSString stringWithUTF8String:state] stringByAppendingPathComponent:@"game.stdout.log"];
    NSString *stderrPath=[[NSString stringWithUTF8String:state] stringByAppendingPathComponent:@"game.stderr.log"];
    posix_spawn_file_actions_addopen(&actions,STDIN_FILENO,"/dev/null",O_RDONLY,0);
    posix_spawn_file_actions_addopen(&actions,STDOUT_FILENO,stdoutPath.fileSystemRepresentation,O_WRONLY|O_CREAT|O_APPEND,0600);
    posix_spawn_file_actions_addopen(&actions,STDERR_FILENO,stderrPath.fileSystemRepresentation,O_WRONLY|O_CREAT|O_APPEND,0600);
    const char *cwd=getenv("SKINOS_GAME_WORKING_DIR");
    if (cwd) posix_spawn_file_actions_addchdir_np(&actions,cwd);
    pid_t pid=0;
    int result=posix_spawn(&pid,executable.fileSystemRepresentation,&actions,NULL,argv,envp);
    posix_spawn_file_actions_destroy(&actions);
    for (NSUInteger i=0;argv[i];i++) free(argv[i]);
    for (NSUInteger i=0;envp[i];i++) free(envp[i]);
    free(argv);free(envp);
    if (result!=0) return rejected(error,"native_spawn_failed");
    last_game_executable=executable;
    atomic_store(&last_game_pid,pid);
    fprintf(stderr,"SKINOS_LAUNCH spawned pid=%d appid=%s\n",pid,expected);fflush(stderr);
    SkinOsChildProcess *process=[SkinOsChildProcess new];
    process.processIdentifier=pid;
    return process;
}

static id native_open_url(id object,SEL selector,NSURL *url,NSWorkspaceLaunchOptions options,NSDictionary *configuration,NSError **error) {
    return native_launch(object,selector,url,options,configuration,error);
}
static id native_open_urls(id object,SEL selector,NSArray *urls,NSURL *application,NSWorkspaceLaunchOptions options,NSDictionary *configuration,NSError **error) {
    if (urls.count) return rejected(error,"document_or_url_launch_not_supported");
    return native_launch(object,selector,application,options,configuration,error);
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
