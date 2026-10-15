#ifndef SKINOS_DINPUT_EXPECT_H
#define SKINOS_DINPUT_EXPECT_H
#include <stdint.h>
typedef struct SkinosDiState {
    int32_t x,y,z,rx,ry;
    uint32_t pov;
    uint8_t buttons[10],padding[2];
} SkinosDiState;
_Static_assert(sizeof(SkinosDiState)==36,"explicit DirectInput data format size");
static inline int skinos_di_i16(const uint8_t *p){unsigned x=p[0]|((unsigned)p[1]<<8);return x&32768?(int)x-65536:(int)x;}
/* Independent semantic oracle for the stock winexinput.sys DirectInput child.
 * This intentionally does not call the producer's HID adapter. Source pinned
 * at ValveSoftware/wine@290276fb04e812b5c4c2338ba0b8b4bd7e4bef4c:
 * winexinput.sys/main.c translate_report_to_xinput_state; joystick_hid.c
 * scale_axis_value, init_object_properties. DI range is explicitly0..65535,
 * deadzone0/saturation10000 and verified through GetProperty by the consumer.
 * The legacy IG child has5 axes: X/Y/Rx/Ry and COMBINED Z trigger. It cannot
 * distinguish simultaneous equal triggers, and does not expose Guide.
 * Known source-level concern: pinned Wine's unsigned -Y-1 before sign_extend
 * mis-scales signed16 reports. These expectations preserve intended semantics
 * so an actual wrong0 read fails; they do not bless that bug as expected input.
 */
static inline void skinos_di_expect(const uint8_t source[20],SkinosDiState *s){
    unsigned b=source[2]|((unsigned)source[3]<<8);
    s->x=skinos_di_i16(source+6)+32768;s->y=32767-skinos_di_i16(source+8);
    s->rx=skinos_di_i16(source+10)+32768;s->ry=32767-skinos_di_i16(source+12);
    s->z=32768+128*((int)source[4]-(int)source[5]);
    static const unsigned bits[10]={12,13,14,15,8,9,5,4,6,7};
    for(unsigned i=0;i<10;i++)s->buttons[i]=(b&(1u<<bits[i]))?128:0;
    int x=((b&8)!=0)-((b&4)!=0),y=((b&2)!=0)-((b&1)!=0);
    if(!x&&!y)s->pov=0xffffffffu;
    else if(y<0)s->pov=x<0?31500:x>0?4500:0;
    else if(y>0)s->pov=x<0?22500:x>0?13500:18000;
    else s->pov=x<0?27000:9000;
    s->padding[0]=s->padding[1]=0;
}
static inline int skinos_di_equal(const SkinosDiState*a,const SkinosDiState*b){
    if(a->x!=b->x||a->y!=b->y||a->z!=b->z||a->rx!=b->rx||a->ry!=b->ry||a->pov!=b->pov)return 0;
    for(unsigned i=0;i<10;i++)if(a->buttons[i]!=b->buttons[i])return 0;
    return 1;
}
static inline unsigned skinos_di_center_rounding(const SkinosDiState *actual,const uint8_t source[20]){
    unsigned mask=0;
    if(skinos_di_i16(source+8)==0&&actual->y==32768)mask|=1;
    if(skinos_di_i16(source+12)==0&&actual->ry==32768)mask|=2;
    return mask;
}
static inline int skinos_di_equal_semantic(const SkinosDiState*actual,const SkinosDiState*expected,const uint8_t source[20]){
    /* dinput/device.c reset_object_value rounds range centre32767.5 upward.
     * SetProperty(DIPROP_RANGE) invokes that reset before acquisition. The
     * processed invertedY centre is32767. Both are neutral only for original
     * signedY=0. No tolerance for edges/nonzero inputs or any other field. */
    SkinosDiState normalized=*actual;unsigned rounding=skinos_di_center_rounding(actual,source);
    if(rounding&1)normalized.y=32767;if(rounding&2)normalized.ry=32767;
    return skinos_di_equal(&normalized,expected);
}
#endif
