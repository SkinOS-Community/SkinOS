import importlib.util
from pathlib import Path
import struct
import unittest

SPEC = importlib.util.spec_from_file_location('native_delivery', Path(__file__).parents[1]/'prepare-native-delivery.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def native_fixture(code=b'EXECUTABLE_BYTES', overlapping_signature=False):
    commands_size = 152 + (16 if overlapping_signature else 0)
    start = 32 + commands_size
    header = struct.pack('<8I',0xfeedfacf,0x0100000c,0,6,2 if overlapping_signature else 1,commands_size,0,0)
    segment = struct.pack('<II16sQQQQIIII',0x19,152,b'__TEXT',0,start+len(code),0,start+len(code),7,5,1,0)
    section = struct.pack('<16s16sQQ8I',b'__text',b'__TEXT',start,len(code),start,0,0,0,0x80000400,0,0,0)
    signature = struct.pack('<4I',0x1d,16,start,len(code)) if overlapping_signature else b''
    return header+segment+section+signature+code


class NativeDeliveryTests(unittest.TestCase):
    def test_metadata_only_recipe_copies_code(self):
        original=native_fixture()
        record=MODULE.recipe(original,original,'NativeRuntimeCode/fixture.dylib')
        self.assertEqual(MODULE.restore(record,original),original)
        self.assertEqual(sum(c['length'] for c in record['chunks'] if 'length' in c),16)

    def test_signer_cannot_change_executable_bytes(self):
        with self.assertRaisesRegex(ValueError,'changed code/data'):
            MODULE.recipe(native_fixture(),native_fixture(b'CHANGED_CODE____'),'NativeRuntimeCode/fixture.dylib')

    def test_signature_cannot_relabel_code_as_metadata(self):
        with self.assertRaisesRegex(ValueError,'overlaps executable code'):
            MODULE.recipe(native_fixture(overlapping_signature=True),native_fixture(b'CHANGED_CODE____',True),
                          'NativeRuntimeCode/fixture.dylib')


if __name__=='__main__':
    unittest.main()
