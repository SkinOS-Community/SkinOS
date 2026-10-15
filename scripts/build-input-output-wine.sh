#!/bin/bash
# Builds only our separate output component and offline tests; no staging into
# any runtime, no original Valve files changed, no HID/Steam/game execution.
set -euo pipefail
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-15.0}"
task_root="$(cd "$(dirname "$0")/.." && pwd)"
task_output="$task_root/target/input-output-wine"
mkdir -p "$task_output"
task_source="$task_root/crates/runtime-host/native/input_output_wine.cpp"
/usr/bin/clang++ -std=c++17 -Wall -Wextra -Werror -fno-exceptions -fno-rtti \
  -DSKINOS_INPUT_OUTPUT_TEST "$task_source" -o "$task_output/input-output-test"
"$task_output/input-output-test"
/usr/bin/clang++ -std=c++17 -Wall -Wextra -Werror -fno-exceptions -fno-rtti \
  -arch arm64 -arch x86_64 -dynamiclib -fvisibility=hidden \
  -Wl,-install_name,@rpath/libsteaminput_wine.dylib \
  "$task_source" -o "$task_output/libsteaminput_wine.dylib"
/usr/bin/codesign --force --sign - "$task_output/libsteaminput_wine.dylib"
/usr/bin/shasum -a 256 "$task_output/libsteaminput_wine.dylib"
