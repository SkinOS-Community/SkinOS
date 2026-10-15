#!/usr/bin/env python3
"""Expose native payload code as Developer ID signed Mach-O carrier files.

Only original Mach-O headers, signing metadata and proven alignment padding are
stored as literals. Every code/data byte is copied unchanged from a visible,
signed carrier. Reconstruction must match the existing complete SHA-256 pin.
This operates on a staging app, never the working dist/SkinOS.app.
"""
import argparse
import base64
import gzip
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
MAX_FILE = 128 * 1024 * 1024
VLV_HEADER_ENGINE_SHA = '035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840'
MAGICS = {b'\xce\xfa\xed\xfe', b'\xcf\xfa\xed\xfe', b'\xfe\xed\xfa\xce', b'\xfe\xed\xfa\xcf',
          b'\xca\xfe\xba\xbe', b'\xca\xfe\xba\xbf', b'\xbe\xba\xfe\xca', b'\xbf\xba\xfe\xca'}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def slices(data):
    magic = data[:4]
    if magic not in MAGICS:
        raise ValueError('Not a Mach-O file')
    if magic in (b'\xca\xfe\xba\xbe', b'\xca\xfe\xba\xbf', b'\xbe\xba\xfe\xca', b'\xbf\xba\xfe\xca'):
        endian = '>' if magic[:1] == b'\xca' else '<'
        wide = magic in (b'\xca\xfe\xba\xbf', b'\xbf\xba\xfe\xca')
        count = struct.unpack_from(endian+'I', data, 4)[0]
        if not 1 <= count <= 8:
            raise ValueError('Invalid fat slice count')
        entry_size = 32 if wide else 20
        header_end = 8 + count * entry_size
        result = []
        end = header_end
        for index in range(count):
            values = struct.unpack_from(endian + ('IIQQII' if wide else 'IIIII'), data, 8 + index*entry_size)
            cpu, subtype, offset, size = values[:4]
            if offset < header_end or size < 28 or offset+size > len(data):
                raise ValueError('Invalid fat slice bounds')
            parsed = thin(data[offset:offset+size])
            if (cpu, subtype) != parsed['cpu']:
                raise ValueError('Fat CPU identity differs from slice')
            result.append(dict(parsed, offset=offset, size=size))
        result.sort(key=lambda item: item['offset'])
        for item in result:
            if item['offset'] < end:
                raise ValueError('Overlapping fat slices')
            end = item['offset']+item['size']
        if len({item['cpu'] for item in result}) != len(result):
            raise ValueError('Duplicate fat CPU identity')
        return header_end, result
    return 0, [dict(thin(data), offset=0, size=len(data))]


def thin(data):
    magic = data[:4]
    if magic not in (b'\xce\xfa\xed\xfe', b'\xcf\xfa\xed\xfe', b'\xfe\xed\xfa\xce', b'\xfe\xed\xfa\xcf'):
        raise ValueError('Unsupported Mach-O slice')
    endian = '<' if magic[0] in (0xce, 0xcf) else '>'
    wide = magic in (b'\xcf\xfa\xed\xfe', b'\xfe\xed\xfa\xcf')
    header_size = 32 if wide else 28
    cpu, subtype, filetype, commands, command_bytes = struct.unpack_from(endian+'IIIII', data, 4)
    header_end = header_size + command_bytes
    if commands > 4096 or header_end > len(data):
        raise ValueError('Invalid Mach-O load commands')
    offset = header_size
    signature = None
    backed_sections = []
    executable_ranges = []
    linkedit_ranges = []
    first_data = len(data)
    segment_end = header_end
    for _ in range(commands):
        command, length = struct.unpack_from(endian+'II', data, offset)
        if length < 8 or offset+length > header_end:
            raise ValueError('Invalid Mach-O command bounds')
        if command == 0x1d:
            if length != 16 or signature is not None:
                raise ValueError('Invalid code signature command')
            start, size = struct.unpack_from(endian+'II', data, offset+8)
            if start < header_end or start+size > len(data):
                raise ValueError('Invalid original signature bounds')
            signature = (start, start+size)
        if command in (1, 0x19):
            is64 = command == 0x19
            fixed, section_size = (72, 80) if is64 else (56, 68)
            file_offset, file_size = struct.unpack_from(endian+('QQ' if is64 else 'II'), data, offset+(40 if is64 else 32))
            initial_protection = struct.unpack_from(endian+'I', data, offset+(60 if is64 else 44))[0]
            section_count = struct.unpack_from(endian+'I', data, offset+(64 if is64 else 48))[0]
            if length < fixed or fixed+section_size*section_count > length or file_offset+file_size > len(data):
                raise ValueError('Invalid segment bounds')
            segment_end = max(segment_end, file_offset+file_size)
            if initial_protection & 4 and file_size:
                executable_ranges.append((file_offset,file_offset+file_size))
            if data[offset+8:offset+24].rstrip(b'\0') == b'__LINKEDIT':
                linkedit_ranges.append((file_offset,file_offset+file_size))
            if file_offset:
                first_data = min(first_data, file_offset)
            for index in range(section_count):
                section = offset+fixed+index*section_size
                size = struct.unpack_from(endian+('Q' if is64 else 'I'), data, section+(40 if is64 else 36))[0]
                start = struct.unpack_from(endian+'I', data, section+(48 if is64 else 40))[0]
                flags = struct.unpack_from(endian+'I', data, section+(64 if is64 else 56))[0]
                if size and flags & 0xff not in (1, 0xc, 0x12):
                    if start < header_end or start+size > len(data):
                        raise ValueError('Invalid file-backed section')
                    first_data = min(first_data, start)
                    backed_sections.append((start,start+size))
        offset += length
    if offset != header_end:
        raise ValueError('Load-command byte count differs')
    if signature:
        begin, end = signature
        if any(begin < stop and start < end for start,stop in backed_sections+executable_ranges):
            raise ValueError('Code signature overlaps executable code or file-backed section data')
        if not any(start <= begin and end <= stop for start,stop in linkedit_ranges):
            raise ValueError('Code signature is outside __LINKEDIT')
        if end-begin < 12:
            raise ValueError('Truncated signature SuperBlob')
        magic, length, count = struct.unpack_from('>III',data,begin)
        if magic != 0xfade0cc0 or not 1 <= count <= 128 or not 12+count*8 <= length <= end-begin:
            raise ValueError('Invalid signature SuperBlob')
        blob_ranges = []
        for index in range(count):
            _, position = struct.unpack_from('>II',data,begin+12+index*8)
            if position < 12+count*8 or position+8 > length:
                raise ValueError('Invalid signature blob offset')
            _, blob_size = struct.unpack_from('>II',data,begin+position)
            if blob_size < 8 or position+blob_size > length:
                raise ValueError('Invalid signature blob size')
            blob_ranges.append((position,position+blob_size))
        blob_ranges.sort()
        if any(blob_ranges[i][1] > blob_ranges[i+1][0] for i in range(len(blob_ranges)-1)):
            raise ValueError('Overlapping signature blobs')
    # Padding is allowed only between load commands and the first backed data,
    # or beyond every segment. It must consist entirely of original zero bytes.
    allowed = [(0, header_end)]
    if first_data > header_end and not any(data[header_end:first_data]):
        allowed.append((header_end, first_data))
    if signature:
        allowed.append(signature)
    if segment_end < len(data) and not any(data[segment_end:]):
        allowed.append((segment_end, len(data)))
    allowed.sort()
    merged = []
    for start, end in allowed:
        if merged and start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(merged[-1][1], end))
        else:
            merged.append((start, end))
    return dict(cpu=(cpu, subtype), filetype=filetype, allowed=merged, signature=signature)


def recipe(original, signed, carrier):
    header_end, original_slices = slices(original)
    # This exact audited Valve container has an extra 144-byte VLV header after
    # its 48-byte FAT table. It is outside every declared executable slice.
    # Do not generalize this to unknown nonzero padding or other native files.
    if sha(original) == VLV_HEADER_ENGINE_SHA:
        if (header_end != 48 or original[48:56] != b'VLV\0\x01\0\0\0'
                or sha(original[48:192]) != 'b254c608fb4ce277e5416cfe76f27173d72f535496064b10b99097972c9f1a74'
                or min(s['offset'] for s in original_slices) < 192):
            raise ValueError('Audited Valve container header layout changed')
        header_end = 192
    _, signed_slices = slices(signed)
    by_cpu = {item['cpu']: item for item in signed_slices}
    if set(by_cpu) != {item['cpu'] for item in original_slices}:
        raise ValueError('Signing changed the architecture set')
    chunks = []
    def literal(data):
        if data:
            chunks.append({'data': base64.b64encode(data).decode('ascii')})
    def copy(start, length):
        if length:
            chunks.append({'offset': start, 'length': length})
    def zero_padding(start, end):
        if any(original[start:end]):
            raise ValueError('Nonzero bytes outside native slices are not signing metadata')
        literal(original[start:end])
    position = header_end
    literal(original[:header_end])
    for old in original_slices:
        zero_padding(position, old['offset'])
        new = by_cpu[old['cpu']]
        cursor = 0
        for start, end in old['allowed'] + [(old['size'], old['size'])]:
            if start > cursor:
                expected = original[old['offset']+cursor:old['offset']+start]
                actual = signed[new['offset']+cursor:new['offset']+start] if start <= new['size'] else b''
                if expected != actual:
                    raise ValueError(f'Signer changed code/data outside metadata at CPU {old["cpu"]}, offset {cursor}')
                copy(new['offset']+cursor, start-cursor)
            literal(original[old['offset']+start:old['offset']+end])
            cursor = end
        position = old['offset']+old['size']
    zero_padding(position, len(original))
    record = dict(carrier=carrier, carrier_sha256=sha(signed), sha256=sha(original), size=len(original), chunks=chunks)
    if not any('offset' in chunk for chunk in chunks):
        raise ValueError('Native carrier recipe must copy executable code/data')
    restored = restore(record, signed)
    if restored != original:
        raise ValueError('Carrier recipe does not restore exact original bytes')
    if sum(len(base64.b64decode(chunk['data'])) for chunk in chunks if 'data' in chunk) > 16*1024*1024:
        raise ValueError('Signing metadata exceeds literal budget')
    return record


def restore(record, signed):
    return b''.join(base64.b64decode(chunk['data'], validate=True) if 'data' in chunk else
                    signed[chunk['offset']:chunk['offset']+chunk['length']] for chunk in record['chunks'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, required=True)
    parser.add_argument('--identity', default=None, help='Explicit Developer ID Application identity')
    args = parser.parse_args()
    app = args.app.resolve(strict=True)
    if app == (ROOT/'dist/SkinOS.app').resolve():
        raise ValueError('Use a staging copy, not dist/SkinOS.app')
    if not args.identity or args.identity == '-':
        raise ValueError('An explicit Developer ID identity is required')
    resources = app/'Contents/Resources'
    if (resources/'native-delivery.json').exists() or (resources/'NativeRuntimeCode').exists():
        raise ValueError('Staging app already has native delivery resources')
    pin_bytes = (ROOT/'crates/runtime-host/resources/runtime-payload-pin.json').read_bytes()
    if (resources/'runtime-payload/manifest.json').read_bytes() != pin_bytes:
        raise ValueError('Runtime manifest differs from exact compiled pin')
    pin = json.loads(pin_bytes)
    bridge_manifest = json.loads((resources/'notproton/bridge-manifest.json').read_text())
    delivery = dict(schema_version=1, runtime={}, bridge={})
    statistics = []
    with tempfile.TemporaryDirectory(prefix='.native-delivery-', dir=resources) as temporary:
        stage = Path(temporary)
        carriers = stage/'NativeRuntimeCode';carriers.mkdir()
        def make_carrier(group, name, original):
            if len(original) > MAX_FILE:
                raise ValueError('Native file exceeds size limit')
            _, architectures = slices(original)
            suffix = '.dylib' if all(s['filetype'] == 6 for s in architectures) else '.tool'
            leaf = f'{group}-{sha(original)}{suffix}'
            carrier = carriers/leaf
            if not carrier.exists():
                carrier.write_bytes(original);carrier.chmod(0o755)
                if any(item['signature'] for item in architectures):
                    subprocess.run(['/usr/bin/codesign','--remove-signature',str(carrier)],check=True)
                subprocess.run(['/usr/bin/codesign','--force','--sign',args.identity,'--options','runtime','--timestamp',
                                '--identifier','app.skinos.native.'+group+'.'+sha(original),str(carrier)],check=True)
                subprocess.run(['/usr/bin/codesign','--verify','--strict',str(carrier)],check=True)
            signed = carrier.read_bytes()
            record = recipe(original, signed, 'NativeRuntimeCode/'+leaf)
            delivery[group][name] = record
            literals = sum(len(base64.b64decode(c['data'])) for c in record['chunks'] if 'data' in c)
            statistics.append(dict(group=group,name=name,sha256=sha(original),original_bytes=len(original),
                                   literal_metadata_bytes=literals,copied_code_and_data_bytes=len(original)-literals))
        with gzip.open(resources/'runtime-payload/payload.gz','rb') as source, (stage/'payload.gz').open('wb') as raw:
            with gzip.GzipFile(filename='',mode='wb',fileobj=raw,mtime=0,compresslevel=6) as output:
                for name, entry in pin['files'].items():
                    if entry['size'] > MAX_FILE: raise ValueError('Oversized runtime entry')
                    data = source.read(entry['size'])
                    if len(data) != entry['size'] or sha(data) != entry['sha256']:
                        raise ValueError('Runtime entry does not match pin: '+name)
                    if data[:4] in MAGICS: make_carrier('runtime',name,data)
                    else: output.write(data)
                if source.read(1): raise ValueError('Unexpected trailing runtime bytes')
        bridge_files = []
        for name, expected in bridge_manifest['files'].items():
            path = resources/'notproton/bridge'/(name+'.gz')
            with gzip.open(path,'rb') as source: data = source.read(MAX_FILE+1)
            if len(data)>MAX_FILE or sha(data)!=expected: raise ValueError('Bridge checksum mismatch: '+name)
            if data[:4] in MAGICS:
                make_carrier('bridge',name,data);bridge_files.append(path)
        if len(delivery['runtime']) != 12 or len(delivery['bridge']) != 3:
            raise ValueError('Unexpected native cohort count; review before distributing')
        # Validate transformed stream and every recipe before publishing any change.
        with gzip.open(stage/'payload.gz','rb') as source:
            for name, entry in pin['files'].items():
                data = restore(delivery['runtime'][name], (stage/delivery['runtime'][name]['carrier']).read_bytes()) if name in delivery['runtime'] else source.read(entry['size'])
                if len(data)!=entry['size'] or sha(data)!=entry['sha256']:raise ValueError('Transformed runtime failed verification')
            if source.read(1):raise ValueError('Unexpected transformed data')
        encoded = json.dumps(delivery, sort_keys=True, separators=(',',':'))+'\n'
        if len(encoded)>24*1024*1024:raise ValueError('Delivery manifest exceeds size limit')
        (stage/'native-delivery.json').write_text(encoded)
        carriers.rename(resources/'NativeRuntimeCode')
        (stage/'payload.gz').replace(resources/'runtime-payload/payload.gz')
        for path in bridge_files:path.unlink()
        (stage/'native-delivery.json').rename(resources/'native-delivery.json')
    print(json.dumps(dict(schema_version=1,verified_original_files=statistics,native_runtime_count=len(delivery['runtime']),
                         native_bridge_count=len(delivery['bridge']),visible_carrier_count=len(list((resources/'NativeRuntimeCode').iterdir()))),indent=2))


if __name__ == '__main__':
    main()
