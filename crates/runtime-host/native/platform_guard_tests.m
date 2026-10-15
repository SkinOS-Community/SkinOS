// Only our adapter and pointer inspection: never calls an original launch method.
#import "launch.m"
#include <assert.h>
int main(void) {
    @autoreleasepool {
        setenv("SKINOS_COMPONENT_ROLE","runtime",1);
        initialize_platform_adapter();
        Method entry=class_getInstanceMethod(objc_getClass("NSWorkspace"),sel_registerName("launchApplicationAtURL:options:configuration:error:"));
        assert(method_getImplementation(entry)==(IMP)native_launch);
        NSError *error=nil;
        assert(native_launch(nil,NULL,[NSURL URLWithString:@"steam://run/1942280"],0,nil,&error)==nil);
        assert(error.code==EPERM);
        error=nil;
        assert(native_open_url(nil,NULL,[NSURL URLWithString:@"https://example.com"],0,nil,&error)==nil);
        assert(error.code==EPERM);
        setenv("SKINOS_LAUNCH_APP_ID","1942280",1);
        setenv("SKINOS_GAME_BUNDLE","/nonexistent/SkinOs/Brotato.app",1);
        error=nil;
        assert(native_launch(nil,NULL,[NSURL fileURLWithPath:@"/Applications/Steam.app"],0,nil,&error)==nil);
        assert(error.code==EPERM);
        error=nil;
        assert(native_launch(nil,NULL,[NSURL fileURLWithPath:@"/nonexistent/SkinOs/Brotato.app"],0,@{},&error)==nil);
        assert(error.code==EPERM);
        puts("NSWorkspace replacement installed; URL, Steam.app, and missing-context targets rejected without launch");
    }
}
