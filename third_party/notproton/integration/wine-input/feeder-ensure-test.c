/* Host-only lifecycle policy tests. No Wine, device, SDK or input execution. */
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include "protocol.h"
typedef struct Mock {char calls[12];unsigned count;char fail;uint32_t error;} Mock;
static uint32_t step(Mock*m,char name){m->calls[m->count++]=name;return m->fail==name?m->error:0;}
static uint32_t remove_owned(void*c){return step(c,'D');}
static uint32_t register_payload(void*c){return step(c,'R');}
static uint32_t verify(void*c){return step(c,'V');}
static uint32_t activate(void*c){return step(c,'A');}
static uint32_t feed(void*c){return step(c,'F');}
static const SkinosEnsureOperations ops={remove_owned,register_payload,verify,activate,feed};
static void check(SkinosEnsureSnapshot state,const char*expected,uint32_t error){
    Mock m={0};assert(skinos_ensure_run(&state,&ops,&m)==error);assert(!strcmp(m.calls,expected));
}
int main(void){
    SkinosEnsureSnapshot empty={0,0,1,0,0,0};check(empty,"RVAF",0);
    SkinosEnsureSnapshot matching={1,1,1,1,1,0};check(matching,"VAF",0);
    SkinosEnsureSnapshot old={1,1,1,1,0,0};check(old,"DRVAF",0);
    SkinosEnsureSnapshot busy=old;busy.owner_busy=1;check(busy,"",32);
    busy=matching;busy.owner_busy=1;check(busy,"",32);
    SkinosEnsureSnapshot foreign=matching;foreign.identity_verified=0;check(foreign,"",5);
    SkinosEnsureSnapshot orphan={0,0,1,1,0,0};check(orphan,"",5);
    orphan.file_equal=1;check(orphan,"RVAF",0);
    SkinosEnsureSnapshot partial={0,1,1,0,0,0};check(partial,"DRVAF",0);
    const char *order="DRVAF";
    for(unsigned i=0;i<5;i++){
        Mock m={0};m.fail=order[i];m.error=1000+i;
        assert(skinos_ensure_run(&old,&ops,&m)==1000+i);
        assert(m.count==i+1&&!memcmp(m.calls,order,i+1));
    }
    Mock changed={0};changed.fail='V';changed.error=1306;
    assert(skinos_ensure_run(&matching,&ops,&changed)==1306&&!strcmp(changed.calls,"V"));
    Mock activation={0};activation.fail='A';activation.error=1055;
    assert(skinos_ensure_run(&empty,&ops,&activation)==1055&&!strcmp(activation.calls,"RVA"));
    puts("ensure lifecycle: busy/foreign never mutate; reuse skips registration; owned update orders stop/register/verify/activate/feed; every failure stops propagation");
    return 0;
}
