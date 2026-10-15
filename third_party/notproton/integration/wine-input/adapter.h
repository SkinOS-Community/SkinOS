#ifndef SKINOS_WINE_INPUT_ADAPTER_H
#define SKINOS_WINE_INPUT_ADAPTER_H
/* Presentation adapter only: input is the original Valve mapped report20.
 * Wine's XInput HID reader consumes buttons1..10, vendor-page Guide, hat1..8,
 * signed X/Y/Rx/Ry and unsigned Z/Rz. It does NOT invert Y. The stock
 * winexinput.sys DirectInput child applies its own Y inversion and combined
 * trigger axis. No physical state, Steam handle, or SDK return is synthesized.
 * Source: ValveSoftware/wine@290276fb04e812b5c4c2338ba0b8b4bd7e4bef4c
 * dlls/xinput1_3/main.c read_controller_state, winexinput.sys/main.c translate.
 */
#define SKINOS_WINE_REPORT_BYTES 14u
static const unsigned char skinos_wine_descriptor[] = {
    0x05,0x01,0x09,0x05,0xa1,0x01,
    0x05,0x09,0x19,0x01,0x29,0x0a,0x15,0x00,0x25,0x01,0x75,0x01,0x95,0x0a,0x81,0x02,
    0x06,0x00,0xff,0x09,0x01,0x95,0x01,0x81,0x02,
    0x95,0x05,0x81,0x03,
    0x05,0x01,0x09,0x39,0x15,0x00,0x25,0x08,0x75,0x08,0x95,0x01,0x81,0x02,
    0x09,0x32,0x09,0x35,0x15,0x00,0x26,0xff,0x00,0x75,0x08,0x95,0x02,0x81,0x02,
    0x09,0x30,0x09,0x31,0x09,0x33,0x09,0x34,0x16,0x00,0x80,0x26,0xff,0x7f,
    0x75,0x10,0x95,0x04,0x81,0x02,
    0x75,0x08,0x95,0x01,0x81,0x03,0xc0
};
static inline void skinos_translate_report(const unsigned char in[20], unsigned char out[14]) {
    static const unsigned char bit_for_usage[10]={12,13,14,15,8,9,5,4,6,7};
    unsigned buttons=(unsigned)in[2]|((unsigned)in[3]<<8), mapped=0;
    for(unsigned i=0;i<10;i++)if(buttons&(1u<<bit_for_usage[i]))mapped|=1u<<i;
    if(buttons&0x0400)mapped|=0x0400;
    for(unsigned i=0;i<14;i++)out[i]=0;
    out[0]=(unsigned char)mapped;out[1]=(unsigned char)(mapped>>8);
    /* A hat cannot encode simultaneous opposites. Cancel each opposite pair,
     * retaining any remaining orthogonal direction. No last-pressed guess. */
    int x=((buttons&8)!=0)-((buttons&4)!=0), y=((buttons&2)!=0)-((buttons&1)!=0);
    if(y<0)out[2]=(unsigned char)(x<0?8:x>0?2:1);
    else if(y>0)out[2]=(unsigned char)(x<0?6:x>0?4:5);
    else out[2]=(unsigned char)(x<0?7:x>0?3:0);
    out[3]=in[4];out[4]=in[5];
    for(unsigned i=0;i<8;i++)out[5+i]=in[6+i];
    /* in[0..1], reserved bit11, in[14..19] are padding, never input. */
}
#endif
