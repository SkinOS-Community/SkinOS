"""Offline shell policy checks; never starts Wine or modifies a prefix."""
import importlib.util
import os
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('notproton_build',ROOT/'scripts/build-notproton.py')
builder=importlib.util.module_from_spec(spec);spec.loader.exec_module(builder)


class InputFilterAdapterTests(unittest.TestCase):
    def source(self):
        upstream=(ROOT/'third_party/notproton/upstream/dylib/feats/compat_run.sh').read_text()
        line=next(line for line in upstream.splitlines() if 'controllers_wanted=$(printf' in line and '057e/2006' in line)
        return 'controllers_wanted="054c/09cc"\n'+line+'\nprintf "%s\\n" "$controllers_wanted"\n'

    def policy(self, exact):
        env=os.environ.copy();env.pop('SKINOS_INPUT_FILTER_EXACT',None)
        if exact is not None:env['SKINOS_INPUT_FILTER_EXACT']=exact
        script=builder.exact_controller_filter_adapter(self.source())
        return subprocess.check_output(['/bin/sh','-c',script],env=env,text=True).splitlines()

    def test_owned_policy_hides_only_the_requested_model(self):
        self.assertEqual(self.policy('1'),['054c/09cc'])

    def test_existing_upstream_policy_is_preserved_without_owned_selection(self):
        self.assertEqual(self.policy(None),['054c/09cc','057e/2006','057e/2007','057e/2009'])

    def test_changed_upstream_anchor_requires_review(self):
        with self.assertRaises(RuntimeError):
            builder.exact_controller_filter_adapter(self.source().replace('057e/2009','057e/2010'))


if __name__=='__main__':unittest.main()
