#ifndef SKINOS_OWNED_INPUT_PATH_H
#define SKINOS_OWNED_INPUT_PATH_H
#include <stdint.h>
static inline uint16_t skinos_path_lower(uint16_t c){return c>='A'&&c<='Z'?(uint16_t)(c+32):c;}
static inline int skinos_path_literal(const uint16_t **path,const char*text){
    while(*text){if(skinos_path_lower(**path)!=(unsigned char)*text++)return 0;++*path;}return 1;
}
/* Accept only the full HID interface path for our IG child, including equal
 * decimal slot IDs and the nonzero generation emitted by our bus driver.
 * Product strings are intentionally excluded: winexinput may rename them. */
static inline int skinos_owned_ig_path(const uint16_t *path,unsigned *slot_out){
    const uint16_t*p=path;unsigned slot,serial_slot,generation=0;
    if(!skinos_path_literal(&p,"\\\\?\\hid#vid_045e&pid_028e&ig_"))return 0;
    if(p[0]<'0'||p[0]>'9'||p[1]<'0'||p[1]>'9')return 0;slot=(p[0]-'0')*10+(p[1]-'0');p+=2;
    if(slot>=16||!skinos_path_literal(&p,"#skinos-wine-output-"))return 0;
    if(p[0]<'0'||p[0]>'9'||p[1]<'0'||p[1]>'9')return 0;serial_slot=(p[0]-'0')*10+(p[1]-'0');p+=2;
    if(serial_slot!=slot||*p++!='-')return 0;
    for(unsigned i=0;i<8;i++){
        unsigned c=skinos_path_lower(*p++),v;
        if(c>='0'&&c<='9')v=c-'0';else if(c>='a'&&c<='f')v=c-'a'+10;else return 0;
        generation=(generation<<4)|v;
    }
    if(!generation||!skinos_path_literal(&p,"#{4d1e55b2-f16f-11cf-88cb-001111000030}")||*p)return 0;
    *slot_out=slot;return 1;
}
#endif
