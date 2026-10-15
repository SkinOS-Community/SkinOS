#ifndef SKINOS_LAUNCH_GUARD_H
#define SKINOS_LAUNCH_GUARD_H
#include <stdbool.h>
#include <string.h>
static inline bool skinos_launch_context_valid(const char *expected,const char *appid,const char *gameid) {
    if (!expected || !appid || !gameid || !*expected || strlen(expected)>10) return false;
    for (const char *p=expected;*p;p++) if (*p<'0'||*p>'9') return false;
    if (expected[0]=='0') return false;
    return strcmp(expected,appid)==0 && strcmp(expected,gameid)==0;
}
#endif
