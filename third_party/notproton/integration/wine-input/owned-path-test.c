#include <assert.h>
#include <stdio.h>
#include "owned-path.h"
static int check(const char*s,unsigned expected){
    uint16_t path[260]={0};unsigned n=0,slot=99;while(s[n]){assert(n<259);path[n]=(unsigned char)s[n];++n;}
    int ok=skinos_owned_ig_path(path,&slot);if(ok)assert(slot==expected);return ok;
}
int main(void){
    assert(check("\\\\?\\HID#VID_045E&PID_028E&IG_00#SkinOS-Wine-Output-00-00000001#{4d1e55b2-f16f-11cf-88cb-001111000030}",0));
    assert(check("\\\\?\\hid#vid_045e&pid_028e&ig_15#skinos-wine-output-15-deadbeef#{4d1e55b2-f16f-11cf-88cb-001111000030}",15));
    assert(!check("\\\\?\\hid#vid_045e&pid_028e&ig_00#skinos-wine-output-00-00000000#{4d1e55b2-f16f-11cf-88cb-001111000030}",0));
    assert(!check("\\\\?\\hid#vid_045e&pid_028e&ig_00#skinos-wine-output-01-00000001#{4d1e55b2-f16f-11cf-88cb-001111000030}",0));
    assert(!check("\\\\?\\hid#vid_045e&pid_028e&xi_00#skinos-wine-output-00-00000001#{4d1e55b2-f16f-11cf-88cb-001111000030}",0));
    assert(!check("\\\\?\\hid#vid_045e&pid_028e&ig_16#skinos-wine-output-16-00000001#{4d1e55b2-f16f-11cf-88cb-001111000030}",0));
    assert(!check("\\\\?\\hid#vid_045e&pid_028e&ig_00#skinos-wine-output-00-00000001#{4d1e55b2-f16f-11cf-88cb-001111000030}extra",0));
    assert(!check("\\\\?\\hid#vid_045e&pid_028e&ig_00#foreign-device#{4d1e55b2-f16f-11cf-88cb-001111000030}",0));
    assert(!check("\\\\?\\hid#vid_054c&pid_028e&ig_00#skinos-wine-output-00-00000001#{4d1e55b2-f16f-11cf-88cb-001111000030}",0));
    puts("owned HID path: actual observed path accepted; foreign/slot/generation/interface/trailing data rejected");return 0;
}
