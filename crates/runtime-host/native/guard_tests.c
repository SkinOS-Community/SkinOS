#include "launch_guard.h"
#include <assert.h>
#include <stdio.h>
int main(void) {
    assert(!skinos_launch_context_valid("1942280",NULL,"1942280"));
    assert(!skinos_launch_context_valid("1942280","1942280",NULL));
    assert(!skinos_launch_context_valid("1942280","0","1942280"));
    assert(!skinos_launch_context_valid("1942280","1942280","1"));
    assert(!skinos_launch_context_valid("1942280","1","1942280"));
    assert(!skinos_launch_context_valid("0","0","0"));
    assert(!skinos_launch_context_valid("1942280;open","1942280;open","1942280;open"));
    assert(skinos_launch_context_valid("1942280","1942280","1942280"));
    puts("8 launch context checks passed; no application or Steam code executed");
}
