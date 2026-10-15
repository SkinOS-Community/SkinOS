// All adapter launches are intercepted. A separate fixture tests only our fixed
// shell bootstrap, $0/$@/cwd and exec handoff to this test executable.
#import <AppKit/AppKit.h>
#include <spawn.h>
#include <assert.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
static NSArray<NSString *> *observedArguments;
static NSDictionary<NSString *,NSString *> *observedEnvironment;
static NSString *observedExecutable;
static NSString *observedCwd;
static int observedClose=-1;
static short observedFlags=0;
static pid_t observedGroup=-1;
static int spawnCalls=0;
static int failAttributeStep=0;
static int spawnError=0;
static int initializedAttributes=0;
static int destroyedAttributes=0;
static int fixture_spawn(pid_t *pid,const char *path,const posix_spawn_file_actions_t *actions,const posix_spawnattr_t *attributes,char *const argv[],char *const envp[]) {
    (void)actions;
    spawnCalls++;
    observedFlags=0;observedGroup=-1;
    if (attributes) {
        assert(posix_spawnattr_getflags(attributes,&observedFlags)==0);
        assert(posix_spawnattr_getpgroup(attributes,&observedGroup)==0);
    }
    if (spawnError) return spawnError;
    observedExecutable=[NSString stringWithUTF8String:path];
    NSMutableArray *args=[NSMutableArray array];
    for (int i=0;argv[i];i++) [args addObject:[NSString stringWithUTF8String:argv[i]]];
    observedArguments=args;
    NSMutableDictionary *env=[NSMutableDictionary dictionary];
    for (int i=0;envp[i];i++) {
        NSString *line=[NSString stringWithUTF8String:envp[i]];
        NSRange equal=[line rangeOfString:@"="];
        env[[line substringToIndex:equal.location]]=[line substringFromIndex:equal.location+1];
    }
    observedEnvironment=env;
    *pid=4242;
    return 0;
}
static int fixture_chdir(posix_spawn_file_actions_t *actions,const char *path) {
    observedCwd=[NSString stringWithUTF8String:path];
    return posix_spawn_file_actions_addchdir_np(actions,path);
}
static int fixture_close(posix_spawn_file_actions_t *actions,int fd) {
    observedClose=fd;
    return posix_spawn_file_actions_addclose(actions,fd);
}
static int fixture_attr_init(posix_spawnattr_t *attributes) {
    if (failAttributeStep==1) return EINVAL;
    int result=posix_spawnattr_init(attributes);
    if (!result) initializedAttributes++;
    return result;
}
static int fixture_attr_group(posix_spawnattr_t *attributes,pid_t group) {
    if (failAttributeStep==2) return EINVAL;
    return posix_spawnattr_setpgroup(attributes,group);
}
static int fixture_attr_flags(posix_spawnattr_t *attributes,short flags) {
    if (failAttributeStep==3) return EINVAL;
    return posix_spawnattr_setflags(attributes,flags);
}
static int fixture_attr_destroy(posix_spawnattr_t *attributes) {
    destroyedAttributes++;
    return posix_spawnattr_destroy(attributes);
}
#define posix_spawnattr_init fixture_attr_init
#define posix_spawnattr_setpgroup fixture_attr_group
#define posix_spawnattr_setflags fixture_attr_flags
#define posix_spawnattr_destroy fixture_attr_destroy
#define posix_spawn fixture_spawn
#define posix_spawn_file_actions_addchdir_np fixture_chdir
#define posix_spawn_file_actions_addclose fixture_close
#import "launch.m"
#undef posix_spawn
#undef posix_spawnattr_init
#undef posix_spawnattr_setpgroup
#undef posix_spawnattr_setflags
#undef posix_spawnattr_destroy
#undef posix_spawn_file_actions_addchdir_np
#undef posix_spawn_file_actions_addclose

static void write_macho(NSString *path) {
    [[NSFileManager defaultManager] createDirectoryAtPath:path.stringByDeletingLastPathComponent withIntermediateDirectories:YES attributes:nil error:NULL];
    const unsigned char bytes[]={0xcf,0xfa,0xed,0xfe,7,0,0,1,0,0,0,0};
    assert([[NSData dataWithBytes:bytes length:sizeof(bytes)] writeToFile:path atomically:YES]);
}
static void select_target(NSString *target,NSString *executable,const char *kind) {
    setenv("SKINOS_GAME_BUNDLE",target.fileSystemRepresentation,1);
    setenv("SKINOS_NATIVE_EXECUTABLE",executable.fileSystemRepresentation,1);
    setenv("SKINOS_NATIVE_TARGET_KIND",kind,1);
}
int main(int argc,const char *argv[]) {
    @autoreleasepool {
        if (argc>1 && strcmp(argv[1],"--script-child")==0) {
            NSMutableArray *values=[NSMutableArray arrayWithObject:[[NSFileManager defaultManager] currentDirectoryPath]];
            for (int i=2;i<argc;i++) [values addObject:[NSString stringWithUTF8String:argv[i]]];
            NSData *data=[NSJSONSerialization dataWithJSONObject:values options:0 error:NULL];
            fwrite(data.bytes,1,data.length,stdout);
            return 0;
        }
        setenv("SKINOS_COMPONENT_ROLE","runtime",1);
        initialize_platform_adapter();
        Method entry=class_getInstanceMethod(objc_getClass("NSWorkspace"),sel_registerName("launchApplicationAtURL:options:configuration:error:"));
        assert(method_getImplementation(entry)==(IMP)native_launch);
        NSError *error=nil;
        assert(native_launch(nil,NULL,[NSURL URLWithString:@"steam://run/123"],0,nil,&error)==nil);
        assert(error.code==EPERM);
        assert(native_open_url(nil,NULL,[NSURL URLWithString:@"https://example.com"],0,nil,&error)==nil);
        NSString *root=resolved(NSTemporaryDirectory());
        root=[root stringByAppendingPathComponent:[NSString stringWithFormat:@"skinos-platform-%@",NSUUID.UUID.UUIDString]];
        assert([[NSFileManager defaultManager] createDirectoryAtPath:root withIntermediateDirectories:YES attributes:nil error:NULL]);
        NSString *binary=[root stringByAppendingPathComponent:@"bin/Game"];
        write_macho(binary);
        setenv("SKINOS_LAUNCH_APP_ID","123",1);
        setenv("SKINOS_GAME_INSTALL_ROOT",root.fileSystemRepresentation,1);
        setenv("SKINOS_GAME_WORKING_DIR",root.fileSystemRepresentation,1);
        setenv("SKINOS_RUNTIME_STATE_ROOT",root.fileSystemRepresentation,1);
        setenv("SKINOS_IPC_SERVICE_NAME","dev.skinos.ipctool.fixture",1);
        setenv("SKINOS_ADAPTER_PATH","/fixture/our-adapter.dylib",1);
        int lock=open("/dev/null",O_RDONLY);
        assert(lock>2);
        char fd[32];snprintf(fd,sizeof(fd),"%d",lock);setenv("SKINOS_PROFILE_LOCK_FD",fd,1);
        NSArray *arguments=@[@"two words",@"quote'\"",@"$(must-not-run)"];
        NSData *encoded=[NSJSONSerialization dataWithJSONObject:arguments options:0 error:NULL];
        setenv("SKINOS_GAME_ARGUMENTS_JSON",[[[NSString alloc] initWithData:encoded encoding:NSUTF8StringEncoding] UTF8String],1);
        NSDictionary *context=@{NSWorkspaceLaunchConfigurationEnvironment:@{@"SteamAppId":@"123",@"SteamGameId":@"123",@"SteamClientLaunch":@"1"},NSWorkspaceLaunchConfigurationArguments:@[@"ignored-runtime-default"]};
        select_target(binary,binary,"binary");
        assert(native_launch(nil,NULL,[NSURL fileURLWithPath:binary],0,@{},&error)==nil);
        NSDictionary *wrong=@{NSWorkspaceLaunchConfigurationEnvironment:@{@"SteamAppId":@"124",@"SteamGameId":@"123"}};
        assert(native_launch(nil,NULL,[NSURL fileURLWithPath:binary],0,wrong,&error)==nil);
        assert(native_launch(nil,NULL,[NSURL fileURLWithPath:binary],0,context,&error)!=nil);
        assert([observedExecutable isEqualToString:binary]);
        assert([observedArguments isEqualToArray:[@[binary] arrayByAddingObjectsFromArray:arguments]]);
        assert([observedCwd isEqualToString:root]);
        assert(observedClose==lock);
        assert(!observedEnvironment[@"SKINOS_PROFILE_LOCK_FD"]);
        assert([observedEnvironment[@"SteamAppId"] isEqualToString:@"123"]);
        assert([observedEnvironment[@"DYLD_INSERT_LIBRARIES"] isEqualToString:@"/fixture/our-adapter.dylib"]);
        assert(skinos_last_game_pid()==4242);
        assert(skinos_last_game_group()==0 && observedGroup==-1 && observedFlags==0);
        assert(skinos_last_game_started_microseconds()>0);
        NSString *bundle=[root stringByAppendingPathComponent:@"Demo.app"];
        NSString *bundleBinary=[bundle stringByAppendingPathComponent:@"Contents/MacOS/Demo"];
        write_macho(bundleBinary);
        NSDictionary *info=@{@"CFBundleExecutable":@"Demo",@"CFBundleIdentifier":@"dev.skinos.fixture",@"CFBundlePackageType":@"APPL"};
        assert([info writeToFile:[bundle stringByAppendingPathComponent:@"Contents/Info.plist"] atomically:YES]);
        select_target(bundle,bundleBinary,"bundle");
        assert(native_launch(nil,NULL,[NSURL fileURLWithPath:bundle],0,context,&error)!=nil);
        assert([observedExecutable isEqualToString:bundleBinary]);
        assert(skinos_last_game_group()==0 && observedGroup==-1 && observedFlags==0);
        assert(native_open_urls(nil,NULL,@[[NSURL fileURLWithPath:binary]],[NSURL fileURLWithPath:bundle],0,context,&error)==nil);
        NSString *script=[root stringByAppendingPathComponent:@"launcher with 'quote.sh"];
        NSString *source=@"#!/bin/sh\n[ \"${DYLD_INSERT_LIBRARIES+x}\" = x ] || exit 80\nexec \"$SKINOS_NATIVE_FIXTURE_CHILD\" --script-child \"$0\" \"$@\"\n";
        assert([source writeToFile:script atomically:YES encoding:NSUTF8StringEncoding error:NULL]);
        select_target(script,script,"script");
        setenv("SKINOS_NATIVE_INTERPRETER","/bin/sh",1);
        assert(native_open_urls(nil,NULL,@[[NSURL fileURLWithPath:script]],[NSURL fileURLWithPath:@"/Applications/TextEdit.app"],0,context,&error)!=nil);
        assert([observedExecutable isEqualToString:@"/bin/sh"]);
        assert(([observedArguments isEqualToArray:[@[@"/bin/sh",@"-c",scriptBootstrap,script] arrayByAddingObjectsFromArray:arguments]]));
        assert(strcmp(skinos_last_game_executable(),"/bin/sh")==0);
        assert(observedGroup==0 && observedFlags==POSIX_SPAWN_SETPGROUP);
        assert(skinos_last_game_group()==4242);
        assert(initializedAttributes==destroyedAttributes);
        uint64_t publishedStart=skinos_last_game_started_microseconds();
        int callsBeforeFailure=spawnCalls;
        for (int step=1;step<=3;step++) {
            failAttributeStep=step;
            assert(native_launch(nil,NULL,[NSURL fileURLWithPath:script],0,context,&error)==nil);
            assert(spawnCalls==callsBeforeFailure);
            assert(skinos_last_game_started_microseconds()==publishedStart);
            assert(skinos_last_game_group()==4242);
            assert(initializedAttributes==destroyedAttributes);
        }
        failAttributeStep=0;
        spawnError=EAGAIN;
        assert(native_launch(nil,NULL,[NSURL fileURLWithPath:script],0,context,&error)==nil);
        assert(skinos_last_game_started_microseconds()==publishedStart);
        assert(initializedAttributes==destroyedAttributes);
        spawnError=0;
        assert(native_open_urls(nil,NULL,@[[NSURL fileURLWithPath:script],[NSURL fileURLWithPath:binary]],nil,0,context,&error)==nil);
        assert(native_open_urls(nil,NULL,@[[NSURL fileURLWithPath:binary]],nil,0,context,&error)==nil);
        NSString *escape=[root stringByAppendingPathComponent:@"escaped.sh"];
        assert([[NSFileManager defaultManager] createSymbolicLinkAtPath:escape withDestinationPath:@"/bin/sh" error:NULL]);
        select_target(escape,escape,"script");
        assert(native_launch(nil,NULL,[NSURL fileURLWithPath:escape],0,context,&error)==nil);
        select_target(script,script,"script");
        setenv("SKINOS_GAME_WORKING_DIR","/Applications",1);
        assert(native_launch(nil,NULL,[NSURL fileURLWithPath:script],0,context,&error)==nil);
        setenv("SKINOS_GAME_WORKING_DIR",root.fileSystemRepresentation,1);
        setenv("SKINOS_NATIVE_INTERPRETER","/usr/bin/env",1);
        assert(native_launch(nil,NULL,[NSURL fileURLWithPath:script],0,context,&error)==nil);
        unsetenv("SKINOS_NATIVE_INTERPRETER_ARGUMENT");
        // Prove the bootstrap preserves the script's observable invocation. It
        // executes only this fixture binary, with no Steam runtime or game.
        for (NSString *shell in @[@"/bin/sh",@"/bin/bash"]) {
            NSTask *task=[NSTask new];
            task.executableURL=[NSURL fileURLWithPath:shell];
            task.currentDirectoryURL=[NSURL fileURLWithPath:root];
            task.arguments=[@[@"-c",scriptBootstrap,script] arrayByAddingObjectsFromArray:arguments];
            task.environment=@{@"SKINOS_COMPONENT_ROLE":@"fixture",@"SKINOS_ADAPTER_PATH":@"",@"SKINOS_NATIVE_FIXTURE_CHILD":resolved([NSString stringWithUTF8String:argv[0]])};
            NSPipe *pipe=[NSPipe pipe];task.standardOutput=pipe;
            assert([task launchAndReturnError:&error]);
            NSData *output=[pipe.fileHandleForReading readDataToEndOfFile];
            [task waitUntilExit];
            assert(task.terminationStatus==0);
            id result=[NSJSONSerialization JSONObjectWithData:output options:0 error:NULL];
            assert(([result isEqualToArray:[@[root,script] arrayByAddingObjectsFromArray:arguments]]));
        }
        close(lock);
        assert([[NSFileManager defaultManager] removeItemAtPath:root error:NULL]);
        puts("Native bundle, Mach-O and script plans preserve context/args/cwd; escapes, arbitrary URLs and invalid interpreters rejected; shell exec fixture passed");
    }
}
